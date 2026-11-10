//! Initial prompt construction for the AI agent.
//!
//! This module builds the stable system prompt plus mutable context messages
//! that are sent to the AI model at the start of each conversation.
//!
//! # System prompt resolution
//!
//! The system prompt is resolved from these sources in precedence order
//! (highest to lowest):
//!
//! 1. **`--system-prompt` CLI flag** — explicit user override
//! 2. **Project-level file**: `.cake/system.md` in the working directory
//! 3. **Settings `system_prompt`**: path from `settings.toml` (project or profile)
//! 4. **User-level file**: `system.md` in the user config directory
//!    (typically `~/.config/cake/system.md`)
//! 5. **Built-in default**: `system.md` embedded at compile time
//!
//! The first readable file found wins. Override files replace the default
//! prompt entirely. Empty files are valid (intentional blank prompt).
//! Unreadable files are skipped with a warning.

use std::path::{Path, PathBuf};

use chrono::Local;
use tracing::{debug, warn};

use crate::clients::{
    ExternalServerInfo, ExternalToolInfo, SandboxPolicy,
    format_tool_list_section_with_mcp_and_skill_search, skill_search_enabled, tool_search_enabled,
};
use crate::config::mcp::McpExposure;
use crate::config::toolbox::ToolboxTool;
use crate::config::{AgentsFile, SkillCatalog, SkillCatalogBudget};
use crate::types::Role;

/// Built-in default system prompt, embedded at compile time from `system.md`.
const BUILTIN_SYSTEM_PROMPT: &str = include_str!("system.md");

/// Instructions for a run whose reader is the `Read` tool.
const SKILL_USAGE_INSTRUCTIONS_READ: &str = r"<skill_instructions>
The following skills provide specialized instructions for specific tasks.
When a task matches a skill's description, use your file-read tool to load
the SKILL.md at the listed location before proceeding.
When a skill references relative paths, resolve them against the skill's
directory (the parent of SKILL.md) and use absolute paths in tool calls.
</skill_instructions>";

/// Instructions for a run whose only reader is a shell-capable tool.
///
/// The prose names `Bash`, so it must change with `SHELL_TOOL_NAMES` when that
/// list admits a second tool.
const SKILL_USAGE_INSTRUCTIONS_SHELL: &str = r"<skill_instructions>
The following skills provide specialized instructions for specific tasks.
When a task matches a skill's description, use the Bash tool to load the
SKILL.md at the listed location with `cat <location>`, quoting the path for
the shell, before proceeding.
When a skill references relative paths, resolve them against the skill's
directory (the parent of SKILL.md) and use absolute paths in tool calls.
</skill_instructions>";

/// Additional instructions when the bounded catalog can omit entries.
const SKILL_SEARCH_HINT: &str = "\nWhen the catalog reports omitted skills, use the SkillSearch tool to find them by name or description, then read the returned SKILL.md location.";

/// Built-in tools that can read a `SKILL.md` without the `Read` tool.
const SHELL_TOOL_NAMES: &[&str] = &["Bash"];

/// How a run can load a skill body, in preference order.
///
/// The catalog is disclosed only when this resolves; without a reader the model
/// has no way to act on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SkillReader {
    /// The `Read` tool, which takes a single file path.
    Read,
    /// A shell-capable tool, which reads a `SKILL.md` through the shell.
    Shell,
}

impl SkillReader {
    /// Resolves the reader for a run. `None` means the full tool registry, which
    /// includes `Read`; an explicit selection with neither reader yields `None`.
    fn resolve(enabled_tools: Option<&[String]>) -> Option<Self> {
        let Some(enabled) = enabled_tools else {
            return Some(Self::Read);
        };
        if enabled.iter().any(|name| name == "Read") {
            Some(Self::Read)
        } else if enabled
            .iter()
            .any(|name| SHELL_TOOL_NAMES.contains(&name.as_str()))
        {
            Some(Self::Shell)
        } else {
            None
        }
    }

    /// Instructions that match how this run loads a skill body.
    const fn instructions(self) -> &'static str {
        match self {
            Self::Read => SKILL_USAGE_INSTRUCTIONS_READ,
            Self::Shell => SKILL_USAGE_INSTRUCTIONS_SHELL,
        }
    }
}

#[derive(Clone, Copy)]
enum PromptSource {
    Cli,
    Project,
    Settings,
    User,
}

impl PromptSource {
    fn log_success(self, path: &Path) {
        match self {
            Self::Cli => debug!("Using --system-prompt file: {}", path.display()),
            Self::Project => debug!("Using project-level system prompt: {}", path.display()),
            Self::Settings => debug!("Using settings system prompt from {}", path.display()),
            Self::User => debug!("Using user-level system prompt: {}", path.display()),
        }
    }

    fn log_failure(self, path: &Path, error: &std::io::Error) {
        match self {
            Self::Cli => warn!(
                "Skipping unreadable --system-prompt file at {}: {error}",
                path.display()
            ),
            Self::Project => warn!(
                "Skipping unreadable system prompt file at {}: {error}",
                path.display()
            ),
            Self::Settings => warn!(
                "Skipping unreadable settings system prompt at {}: {error}",
                path.display()
            ),
            Self::User => warn!(
                "Skipping unreadable system prompt file at {}: {error}",
                path.display()
            ),
        }
    }
}

fn read_prompt_override(path: &Path, source: PromptSource) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            source.log_success(path);
            Some(content.trim().to_string())
        },
        Err(error) => {
            source.log_failure(path, &error);
            None
        },
    }
}

/// The source that supplied the system prompt for a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemPromptSource {
    Cli,
    Project,
    Settings,
    User,
    Builtin,
}

impl SystemPromptSource {
    /// Stable label for diagnostic output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Project => "project",
            Self::Settings => "settings",
            Self::User => "user",
            Self::Builtin => "builtin",
        }
    }
}

/// The selected system prompt source and the file that supplied it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSystemPromptSource {
    pub source: SystemPromptSource,
    pub path: Option<PathBuf>,
}

/// Resolve which source supplies the system prompt without returning its text.
///
/// Precedence and readability checks match [`resolve_system_prompt_with_mcp`].
/// The prompt body is read only to make the same readability decision and is
/// dropped; no caller can print it from this value.
#[must_use]
pub fn resolve_system_prompt_source(
    working_dir: &Path,
    config_dir: &Path,
    cli_system_prompt: Option<&Path>,
    settings_system_prompt: Option<&Path>,
    include_project_prompt: bool,
) -> ResolvedSystemPromptSource {
    if let Some(path) = cli_system_prompt
        && read_prompt_override(path, PromptSource::Cli).is_some()
    {
        return ResolvedSystemPromptSource {
            source: SystemPromptSource::Cli,
            path: Some(path.to_path_buf()),
        };
    }

    if include_project_prompt
        && let Some(path) = project_prompt_path(working_dir)
        && read_prompt_override(&path, PromptSource::Project).is_some()
    {
        return ResolvedSystemPromptSource {
            source: SystemPromptSource::Project,
            path: Some(path),
        };
    }

    if let Some(path) = settings_system_prompt
        && read_prompt_override(path, PromptSource::Settings).is_some()
    {
        return ResolvedSystemPromptSource {
            source: SystemPromptSource::Settings,
            path: Some(path.to_path_buf()),
        };
    }

    let user_path = config_dir.join("system.md");
    if user_path.exists() && read_prompt_override(&user_path, PromptSource::User).is_some() {
        return ResolvedSystemPromptSource {
            source: SystemPromptSource::User,
            path: Some(user_path),
        };
    }

    ResolvedSystemPromptSource {
        source: SystemPromptSource::Builtin,
        path: None,
    }
}

/// Resolves the system prompt from CLI flags, override files, settings, or the built-in default.
///
/// Checks sources in precedence order (highest to lowest):
/// 1. `--system-prompt` CLI flag (readable file at given path)
/// 2. Project-level file: `working_dir/.cake/system.md`
/// 3. Settings `system_prompt`: path from `settings.toml` (project or profile)
/// 4. User-level file: `config_dir/system.md`
///
/// If none of these sources produce a readable file, the built-in default is used.
/// The first readable file found wins. Empty files are valid (intentional blank prompt).
/// Unreadable files produce a warning and are skipped. The optional `enabled_tools`
/// list filters the built-in available-tools section by exact registered name.
/// `supports_images` reports whether the resolved model accepts image input; it
/// gates tools such as `ReadImage` that send pixels to the provider.
#[expect(
    clippy::too_many_arguments,
    reason = "prompt resolution naturally requires many inputs"
)]
#[cfg(test)]
pub fn resolve_system_prompt(
    working_dir: &Path,
    config_dir: &Path,
    cli_system_prompt: Option<&Path>,
    settings_system_prompt: Option<&Path>,
    sandbox_policy: SandboxPolicy,
    toolbox_tools: &[ToolboxTool],
    enabled_tools: Option<&[String]>,
    supports_images: bool,
) -> String {
    resolve_system_prompt_with_project(
        working_dir,
        config_dir,
        cli_system_prompt,
        settings_system_prompt,
        sandbox_policy,
        toolbox_tools,
        enabled_tools,
        supports_images,
        true,
    )
}

/// Resolve the system prompt, optionally skipping automatic project content.
///
/// `include_project_prompt` is false under `--no-project-config`. The automatic
/// `.cake/system.md` source is also skipped when it is a symlink, even in a
/// trusted project, so project prompt content cannot read a host file through a
/// link.
#[expect(
    clippy::too_many_arguments,
    reason = "prompt resolution naturally requires many inputs"
)]
#[cfg(test)]
pub fn resolve_system_prompt_with_project(
    working_dir: &Path,
    config_dir: &Path,
    cli_system_prompt: Option<&Path>,
    settings_system_prompt: Option<&Path>,
    sandbox_policy: SandboxPolicy,
    toolbox_tools: &[ToolboxTool],
    enabled_tools: Option<&[String]>,
    supports_images: bool,
    include_project_prompt: bool,
) -> String {
    resolve_system_prompt_with_mcp(
        working_dir,
        config_dir,
        cli_system_prompt,
        settings_system_prompt,
        sandbox_policy,
        toolbox_tools,
        &[],
        enabled_tools,
        supports_images,
        false,
        include_project_prompt,
    )
}

/// Resolve the system prompt with MCP server tools included in the list.
#[expect(
    clippy::too_many_arguments,
    reason = "prompt resolution naturally requires many inputs"
)]
pub fn resolve_system_prompt_with_mcp(
    working_dir: &Path,
    config_dir: &Path,
    cli_system_prompt: Option<&Path>,
    settings_system_prompt: Option<&Path>,
    sandbox_policy: SandboxPolicy,
    toolbox_tools: &[ToolboxTool],
    mcp_servers: &[crate::clients::mcp::McpServer],
    enabled_tools: Option<&[String]>,
    supports_images: bool,
    has_skill_search: bool,
    include_project_prompt: bool,
) -> String {
    // 1. --system-prompt CLI flag
    if let Some(path) = cli_system_prompt
        && let Some(prompt) = read_prompt_override(path, PromptSource::Cli)
    {
        return prompt;
    }

    // 2. Project-level file: .cake/system.md
    if include_project_prompt && let Some(prompt) = read_project_prompt(working_dir) {
        return prompt;
    }

    // 3. Settings system_prompt path
    if let Some(path) = settings_system_prompt
        && let Some(prompt) = read_prompt_override(path, PromptSource::Settings)
    {
        return prompt;
    }

    // 4. User-level file: ~/.config/cake/system.md
    let user_path = config_dir.join("system.md");
    if user_path.exists()
        && let Some(prompt) = read_prompt_override(&user_path, PromptSource::User)
    {
        return prompt;
    }

    debug!("Using built-in system prompt (no override found)");
    builtin_prompt(
        sandbox_policy,
        toolbox_tools,
        mcp_servers,
        enabled_tools,
        supports_images,
        has_skill_search,
    )
}

/// Read the automatic project prompt, skipping a symlink rather than following
/// it into an arbitrary host path.
fn read_project_prompt(working_dir: &Path) -> Option<String> {
    let path = project_prompt_path(working_dir)?;
    read_prompt_override(&path, PromptSource::Project)
}

/// Resolve the project prompt candidate, rejecting symlinks and non-files.
fn project_prompt_path(working_dir: &Path) -> Option<PathBuf> {
    let project_path = working_dir.join(".cake").join("system.md");
    match std::fs::symlink_metadata(&project_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            warn!(
                path = %project_path.display(),
                "skipping symlinked project system prompt"
            );
            None
        },
        Ok(metadata) if metadata.is_file() => Some(project_path),
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            warn!(
                path = %project_path.display(),
                error = %error,
                "could not inspect project system prompt"
            );
            None
        },
    }
}

/// Render the built-in prompt, filling the tool list when the template has it.
fn builtin_prompt(
    sandbox_policy: SandboxPolicy,
    toolbox_tools: &[ToolboxTool],
    mcp_servers: &[crate::clients::mcp::McpServer],
    enabled_tools: Option<&[String]>,
    supports_images: bool,
    has_skill_search: bool,
) -> String {
    let builtin = BUILTIN_SYSTEM_PROMPT.trim().to_string();
    if builtin.contains("{{AVAILABLE_TOOLS}}") {
        let mcp_server_infos: Vec<ExternalServerInfo> = mcp_servers
            .iter()
            .filter(|server| server.exposure != McpExposure::Hidden)
            .filter_map(|server| {
                let enabled_tool_count = server
                    .tools
                    .iter()
                    .filter(|tool| tool_enabled(&tool.registered_name, enabled_tools))
                    .count();
                (enabled_tool_count > 0).then(|| ExternalServerInfo {
                    name: server.name.clone(),
                    description: server.description.clone(),
                    exposure: effective_mcp_exposure(server.exposure, enabled_tools),
                    tool_count: server.tools.len(),
                    enabled_tool_count,
                })
            })
            .collect();
        let mcp_tools: Vec<ExternalToolInfo> = mcp_servers
            .iter()
            .filter(|server| {
                effective_mcp_exposure(server.exposure, enabled_tools) == McpExposure::Direct
            })
            .flat_map(|server| server.tools.iter())
            .filter(|tool| tool_enabled(&tool.registered_name, enabled_tools))
            .map(|tool| ExternalToolInfo {
                name: tool.registered_name.clone(),
                description: tool.description.clone(),
            })
            .collect();
        builtin.replace(
            "{{AVAILABLE_TOOLS}}",
            &format_tool_list_section_with_mcp_and_skill_search(
                sandbox_policy,
                toolbox_tools,
                &mcp_server_infos,
                &mcp_tools,
                enabled_tools,
                supports_images,
                has_skill_search,
            ),
        )
    } else {
        builtin
    }
}

/// Whether an exact-name allowlist permits a tool. `None` permits every tool.
fn tool_enabled(name: &str, enabled_tools: Option<&[String]>) -> bool {
    enabled_tools.is_none_or(|names| names.iter().any(|enabled| enabled == name))
}

/// A deferred server becomes direct when the allowlist excludes `ToolSearch`,
/// because that is the only surface that can load its tools.
fn effective_mcp_exposure(exposure: McpExposure, enabled_tools: Option<&[String]>) -> McpExposure {
    if exposure == McpExposure::Deferred && !tool_search_enabled(enabled_tools) {
        McpExposure::Direct
    } else {
        exposure
    }
}

/// Builds all initial prompt messages for the AI agent.
///
/// The first message is the stable system prompt. Mutable context such as
/// AGENTS.md contents, available skills, and environment context is emitted as
/// separate developer messages so it is not tied to the system prompt. The
/// optional exact-name tool allowlist narrows which tools are advertised to the
/// model; `None` preserves the full registry. `supports_images` gates tools that
/// send image input to the provider. `include_runtime_context` controls the
/// final Developer message that describes the process environment.
#[expect(
    clippy::too_many_arguments,
    reason = "prompt construction naturally requires many inputs"
)]
#[cfg(test)]
pub fn build_initial_prompt_messages_with_enabled_tools(
    working_dir: &Path,
    config_dir: &Path,
    cli_system_prompt: Option<&Path>,
    settings_system_prompt: Option<&Path>,
    agents_files: &[AgentsFile],
    skill_catalog: &SkillCatalog,
    sandbox_policy: SandboxPolicy,
    toolbox_tools: &[ToolboxTool],
    enabled_tools: Option<&[String]>,
    supports_images: bool,
    include_runtime_context: bool,
) -> Vec<(Role, String)> {
    build_initial_prompt_messages_with_project(
        working_dir,
        config_dir,
        cli_system_prompt,
        settings_system_prompt,
        agents_files,
        skill_catalog,
        sandbox_policy,
        toolbox_tools,
        enabled_tools,
        supports_images,
        include_runtime_context,
        true,
    )
}

/// Build initial prompt messages, optionally skipping automatic project content.
#[expect(
    clippy::too_many_arguments,
    reason = "prompt construction naturally requires many inputs"
)]
#[cfg(test)]
pub fn build_initial_prompt_messages_with_project(
    working_dir: &Path,
    config_dir: &Path,
    cli_system_prompt: Option<&Path>,
    settings_system_prompt: Option<&Path>,
    agents_files: &[AgentsFile],
    skill_catalog: &SkillCatalog,
    sandbox_policy: SandboxPolicy,
    toolbox_tools: &[ToolboxTool],
    enabled_tools: Option<&[String]>,
    supports_images: bool,
    include_runtime_context: bool,
    include_project_prompt: bool,
) -> Vec<(Role, String)> {
    build_initial_prompt_messages_with_mcp(
        working_dir,
        config_dir,
        cli_system_prompt,
        settings_system_prompt,
        agents_files,
        skill_catalog,
        SkillCatalogBudget::defaults(),
        sandbox_policy,
        toolbox_tools,
        &[],
        enabled_tools,
        supports_images,
        include_runtime_context,
        include_project_prompt,
    )
}

/// Build initial prompt messages with MCP server tools in the tool list.
#[expect(
    clippy::too_many_arguments,
    reason = "prompt construction naturally requires many inputs"
)]
pub fn build_initial_prompt_messages_with_mcp(
    working_dir: &Path,
    config_dir: &Path,
    cli_system_prompt: Option<&Path>,
    settings_system_prompt: Option<&Path>,
    agents_files: &[AgentsFile],
    skill_catalog: &SkillCatalog,
    skill_budget: SkillCatalogBudget,
    sandbox_policy: SandboxPolicy,
    toolbox_tools: &[ToolboxTool],
    mcp_servers: &[crate::clients::mcp::McpServer],
    enabled_tools: Option<&[String]>,
    supports_images: bool,
    include_runtime_context: bool,
    include_project_prompt: bool,
) -> Vec<(Role, String)> {
    let skill_search_available = skill_search_enabled(enabled_tools)
        && !skill_catalog.skills.is_empty()
        && SkillReader::resolve(enabled_tools).is_some();
    let mut messages = vec![(
        Role::System,
        resolve_system_prompt_with_mcp(
            working_dir,
            config_dir,
            cli_system_prompt,
            settings_system_prompt,
            sandbox_policy,
            toolbox_tools,
            mcp_servers,
            enabled_tools,
            supports_images,
            skill_search_available,
            include_project_prompt,
        ),
    )];
    let context = format_agents_context(agents_files);
    if !context.is_empty() {
        messages.push((Role::Developer, context));
    }

    if let Some(reader) = SkillReader::resolve(enabled_tools)
        && !skill_catalog.skills.is_empty()
    {
        // Omission is allowed only when the run can search the complete
        // catalog. Without SkillSearch the catalog stays complete and the
        // over-budget warning names the missing search surface.
        let search_available = skill_search_available;
        let render = skill_catalog.to_prompt_xml_with_budget(skill_budget, search_available);
        let metrics = render.metrics();
        if render.over_budget {
            warn!(
                catalog_characters = metrics.characters,
                budget_characters = skill_budget.catalog_characters,
                shortened_descriptions = render.shortened_descriptions,
                omitted_skills = render.omitted_skills.len(),
                search_available,
                "skill catalog exceeds its resolved budget"
            );
        }
        if !render.xml.is_empty() {
            let instructions = reader.instructions();
            let search_hint = if search_available {
                SKILL_SEARCH_HINT
            } else {
                ""
            };
            messages.push((
                Role::Developer,
                format!("## Skills\n\n{instructions}{search_hint}\n\n{}", render.xml),
            ));
        }
    }

    if include_runtime_context {
        let today = Local::now().format("%Y-%m-%d").to_string();
        let working_dir_str = working_dir.to_string_lossy();
        let platform = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_string());
        let terminal = std::env::var("TERM_PROGRAM")
            .or_else(|_| std::env::var("TERM"))
            .unwrap_or_else(|_| "unknown".to_string());
        let env_message = format!(
            "Current working directory: {working_dir_str}\n\
             Today's date: {today}\n\
             Platform: {platform}\n\
             Architecture: {arch}\n\
             Shell: {shell}\n\
             Terminal: {terminal}"
        );
        messages.push((Role::Developer, env_message));
    }

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
    use crate::config::skills::{Skill, SkillScope, discover_skills};
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
            filters => vec![
                (r"Today's date: \d{4}-\d{2}-\d{2}", "Today's date: [DATE]"),
                (r"Platform: .+", "Platform: [PLATFORM]"),
                (r"Architecture: .+", "Architecture: [ARCH]"),
                (r"Shell: .+", "Shell: [SHELL]"),
                (r"Terminal: .+", "Terminal: [TERMINAL]"),
            ]
        }, {
            insta::assert_snapshot!(name, prompt);
        });
    }

    // --- resolve_system_prompt tests ---

    #[test]
    fn resolve_builtin_gates_read_image_on_model_capability() {
        let dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        let capable = resolve_system_prompt(
            dir.path(),
            config_dir.path(),
            None,
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
        );
        assert!(capable.contains("- **ReadImage**:"));

        let text_only = resolve_system_prompt(
            dir.path(),
            config_dir.path(),
            None,
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            false,
        );
        assert!(
            !text_only.contains("- **ReadImage**:"),
            "a text-only model's prompt must not list ReadImage"
        );
        assert!(text_only.contains("- **Read**:"));
    }

    #[test]
    fn resolve_builtin_filters_enabled_tools() {
        let dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();
        let enabled = vec!["Read".to_string()];
        let prompt = resolve_system_prompt(
            dir.path(),
            config_dir.path(),
            None,
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            Some(&enabled),
            true,
        );

        assert!(prompt.contains("- **Read**:"));
        assert!(!prompt.contains("- **Bash**:"));
        assert!(!prompt.contains("- **Edit**:"));
        assert!(!prompt.contains("- **Write**:"));
    }

    #[test]
    fn resolve_uses_cli_override_and_trims() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();
        let cli_file = working_dir.path().join("cli-system.md");
        std::fs::write(&cli_file, "  CLI prompt  \n").unwrap();

        let prompt = resolve_system_prompt(
            working_dir.path(),
            config_dir.path(),
            Some(&cli_file),
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
        );

        assert_eq!(prompt, "CLI prompt");
    }

    #[test]
    fn resolve_uses_settings_override_when_no_higher_precedence_file_exists() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();
        let settings_file = working_dir.path().join("settings-system.md");
        std::fs::write(&settings_file, "Settings prompt").unwrap();

        let prompt = resolve_system_prompt(
            working_dir.path(),
            config_dir.path(),
            None,
            Some(&settings_file),
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
        );

        assert_eq!(prompt, "Settings prompt");
    }

    #[test]
    fn resolve_unreadable_cli_override_falls_back_to_user_level() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();
        let missing_cli_file = working_dir.path().join("missing-cli-system.md");
        std::fs::write(config_dir.path().join("system.md"), "User prompt").unwrap();

        let prompt = resolve_system_prompt(
            working_dir.path(),
            config_dir.path(),
            Some(&missing_cli_file),
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
        );

        assert_eq!(prompt, "User prompt");
    }

    #[test]
    fn resolve_unreadable_settings_override_falls_back_to_builtin() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();
        let missing_settings_file = working_dir.path().join("missing-settings-system.md");

        let prompt = resolve_system_prompt(
            working_dir.path(),
            config_dir.path(),
            None,
            Some(&missing_settings_file),
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
        );

        assert!(prompt.starts_with("You are cake."));
    }

    #[test]
    fn resolve_system_prompt_source_matches_override_precedence() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();
        let settings_file = working_dir.path().join("settings-system.md");
        let cli_file = working_dir.path().join("cli-system.md");

        let selected =
            resolve_system_prompt_source(working_dir.path(), config_dir.path(), None, None, true);
        assert_eq!(selected.source, SystemPromptSource::Builtin);
        assert!(selected.path.is_none());

        std::fs::write(config_dir.path().join("system.md"), "User prompt").unwrap();
        let selected =
            resolve_system_prompt_source(working_dir.path(), config_dir.path(), None, None, true);
        assert_eq!(selected.source, SystemPromptSource::User);

        std::fs::write(&settings_file, "Settings prompt").unwrap();
        let selected = resolve_system_prompt_source(
            working_dir.path(),
            config_dir.path(),
            None,
            Some(&settings_file),
            true,
        );
        assert_eq!(selected.source, SystemPromptSource::Settings);
        assert_eq!(selected.path.as_deref(), Some(settings_file.as_path()));

        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        std::fs::write(cake_dir.join("system.md"), "Project prompt").unwrap();
        let selected = resolve_system_prompt_source(
            working_dir.path(),
            config_dir.path(),
            None,
            Some(&settings_file),
            true,
        );
        assert_eq!(selected.source, SystemPromptSource::Project);

        std::fs::write(&cli_file, "CLI prompt").unwrap();
        let selected = resolve_system_prompt_source(
            working_dir.path(),
            config_dir.path(),
            Some(&cli_file),
            Some(&settings_file),
            true,
        );
        assert_eq!(selected.source, SystemPromptSource::Cli);
        assert_eq!(selected.path.as_deref(), Some(cli_file.as_path()));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_system_prompt_source_skips_a_symlinked_project_prompt() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        let target = external.path().join("host-prompt.md");
        std::fs::write(&target, "Host prompt").unwrap();
        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        std::os::unix::fs::symlink(&target, cake_dir.join("system.md")).unwrap();
        std::fs::write(config_dir.path().join("system.md"), "User prompt").unwrap();

        let selected =
            resolve_system_prompt_source(working_dir.path(), config_dir.path(), None, None, true);

        assert_eq!(selected.source, SystemPromptSource::User);
    }

    #[test]
    fn resolve_uses_project_level_override() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        std::fs::write(cake_dir.join("system.md"), "Project prompt").unwrap();

        let prompt = resolve_system_prompt(
            working_dir.path(),
            config_dir.path(),
            None,
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
        );
        assert_eq!(prompt, "Project prompt");
    }

    #[test]
    fn resolve_skips_project_prompt_when_project_config_is_disabled() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        std::fs::write(cake_dir.join("system.md"), "Project prompt").unwrap();
        std::fs::write(config_dir.path().join("system.md"), "User prompt").unwrap();

        let prompt = resolve_system_prompt_with_project(
            working_dir.path(),
            config_dir.path(),
            None,
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
            false,
        );
        assert_eq!(prompt, "User prompt");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_skips_a_symlinked_project_prompt() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        let target = external.path().join("host-prompt.md");
        std::fs::write(&target, "Host prompt").unwrap();
        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        std::os::unix::fs::symlink(&target, cake_dir.join("system.md")).unwrap();
        std::fs::write(config_dir.path().join("system.md"), "User prompt").unwrap();

        let prompt = resolve_system_prompt_with_project(
            working_dir.path(),
            config_dir.path(),
            None,
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
            true,
        );
        assert_eq!(prompt, "User prompt");
    }

    #[test]
    fn resolve_uses_user_level_override_when_no_project_override() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        std::fs::write(config_dir.path().join("system.md"), "User prompt").unwrap();

        let prompt = resolve_system_prompt(
            working_dir.path(),
            config_dir.path(),
            None,
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
        );
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

        let prompt = resolve_system_prompt(
            working_dir.path(),
            config_dir.path(),
            None,
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
        );
        assert_eq!(prompt, "Project prompt");
    }

    #[test]
    fn resolve_empty_file_is_valid() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        std::fs::write(cake_dir.join("system.md"), "").unwrap();

        let prompt = resolve_system_prompt(
            working_dir.path(),
            config_dir.path(),
            None,
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
        );
        assert_eq!(prompt, "");
    }

    #[test]
    fn resolve_whitespace_only_file_is_trimmed_to_empty() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        std::fs::write(cake_dir.join("system.md"), "   \n\n  ").unwrap();

        let prompt = resolve_system_prompt(
            working_dir.path(),
            config_dir.path(),
            None,
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
        );
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

        let prompt = resolve_system_prompt(
            working_dir.path(),
            config_dir.path(),
            None,
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
        );

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

        let prompt = resolve_system_prompt(
            working_dir.path(),
            config_dir.path(),
            None,
            None,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
        );

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

    // --- build_initial_prompt_messages_with_enabled_tools tests ---

    fn default_config_dir() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn only_user_agents_file() {
        let config_dir = default_config_dir();
        let files = vec![AgentsFile {
            path: "~/.cake/AGENTS.md".to_string(),
            content: "User instructions".to_string(),
        }];
        let messages = build_initial_prompt_messages_with_enabled_tools(
            Path::new("/tmp"),
            config_dir.path(),
            None,
            None,
            &files,
            &SkillCatalog::empty(),
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
            true,
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
        let messages = build_initial_prompt_messages_with_enabled_tools(
            Path::new("/tmp"),
            config_dir.path(),
            None,
            None,
            &files,
            &SkillCatalog::empty(),
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
            true,
        );
        let prompt = render_messages(&messages);
        // Should not include Project Context section since all files are empty
        assert!(!prompt.contains("## Additional Context"));
        // But should still include working directory and date
        assert!(prompt.contains("Current working directory: /tmp"));
        assert!(prompt.contains("Today's date:"));
    }

    #[test]
    fn runtime_context_can_be_omitted_without_changing_other_messages() {
        let config_dir = default_config_dir();
        let files = vec![AgentsFile {
            path: "./AGENTS.md".to_string(),
            content: "Project instructions.".to_string(),
        }];
        let mut catalog = SkillCatalog::empty();
        catalog.skills.push(Skill {
            name: "debugging".to_string(),
            description: "How to debug Rust programs".to_string(),
            location: PathBuf::from("/project/.agents/skills/debugging/SKILL.md"),
            base_directory: PathBuf::from("/project/.agents/skills/debugging"),
            scope: SkillScope::Project,
        });
        let enabled = vec!["Read".to_string()];
        let build = |include_runtime_context: bool| {
            build_initial_prompt_messages_with_enabled_tools(
                Path::new("/project"),
                config_dir.path(),
                None,
                None,
                &files,
                &catalog,
                SandboxPolicy::WorkspaceWrite,
                &[],
                Some(&enabled),
                true,
                include_runtime_context,
            )
        };

        let with_context = build(true);
        let without_context = build(false);

        assert_eq!(with_context.len(), without_context.len() + 1);
        assert_eq!(
            &with_context[..with_context.len() - 1],
            without_context.as_slice()
        );
        assert_eq!(
            with_context.last().map(|(role, _)| *role),
            Some(Role::Developer)
        );
        for label in [
            "Current working directory",
            "Today's date",
            "Platform:",
            "Architecture:",
            "Shell:",
            "Terminal:",
        ] {
            assert!(
                !without_context
                    .iter()
                    .any(|(_, content)| content.contains(label)),
                "runtime label {label:?} survived --no-runtime-context"
            );
            assert!(
                with_context
                    .last()
                    .is_some_and(|(_, content)| content.contains(label)),
                "default runtime message is missing {label:?}"
            );
        }

        let prompt = render_messages(&without_context);
        assert!(prompt.contains("## Available tools"));
        assert!(prompt.contains("## Additional Context"));
        assert!(prompt.contains("Project instructions."));
        assert!(prompt.contains("## Skills"));
        assert!(prompt.contains("<name>debugging</name>"));
    }

    /// Writes one project skill into `working_dir` and returns its catalog.
    fn project_skill_catalog(working_dir: &Path) -> SkillCatalog {
        let skill_dir = working_dir.join(".agents").join("skills").join("debugging");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: debugging\ndescription: How to debug things\n---\n\nInstructions.",
        )
        .unwrap();

        // Discover under a scratch HOME. `discover_skills` also scans the user's
        // `~/.agents/skills`, and reading the developer's real home would make this
        // test's coverage --- and the CRAP baseline it feeds --- depend on the
        // machine (#699). The empty scratch home matches a clean CI runner.
        let home = TempDir::new().unwrap();
        let catalog =
            temp_env::with_var("HOME", Some(home.path()), || discover_skills(working_dir));
        assert!(catalog.skills.iter().any(|skill| skill.name == "debugging"));
        catalog
    }

    /// Builds the prompt for a discovered project skill under `enabled_tools`.
    fn skill_prompt(working_dir: &Path, config_dir: &Path, enabled: &[String]) -> String {
        let catalog = project_skill_catalog(working_dir);
        let messages = build_initial_prompt_messages_with_enabled_tools(
            working_dir,
            config_dir,
            None,
            None,
            &[],
            &catalog,
            SandboxPolicy::WorkspaceWrite,
            &[],
            Some(enabled),
            true,
            true,
        );
        render_messages(&messages)
    }

    #[test]
    fn skill_catalog_disclosed_with_bash_only_reader() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = default_config_dir();

        let prompt = skill_prompt(working_dir.path(), config_dir.path(), &["Bash".to_string()]);

        assert!(prompt.contains("## Skills"));
        assert!(prompt.contains("<name>debugging</name>"));
        assert!(prompt.contains("`cat <location>`"));
        assert!(!prompt.contains("file-read tool"));
    }

    #[test]
    fn skill_catalog_omitted_when_no_reader_is_available() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = default_config_dir();

        for enabled in [
            Vec::new(),
            vec!["Edit".to_string(), "Write".to_string()],
            vec!["tb__run_tests".to_string()],
        ] {
            let prompt = skill_prompt(working_dir.path(), config_dir.path(), &enabled);
            assert!(
                !prompt.contains("## Skills"),
                "catalog disclosed for {enabled:?}"
            );
            assert!(!prompt.contains("<name>debugging</name>"));
        }
    }

    #[test]
    fn skill_catalog_keeps_read_instructions_with_read_tool() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = default_config_dir();

        for enabled in [
            vec!["Read".to_string()],
            vec!["Bash".to_string(), "Read".to_string()],
        ] {
            let prompt = skill_prompt(working_dir.path(), config_dir.path(), &enabled);
            assert!(prompt.contains("<name>debugging</name>"));
            assert!(prompt.contains("use your file-read tool"));
            assert!(!prompt.contains("`cat <location>`"));
        }
    }

    #[test]
    fn budgeted_catalog_omits_only_when_skill_search_is_available() {
        let config_dir = default_config_dir();
        let mut catalog = SkillCatalog::empty();
        for name in ["one", "two", "three"] {
            catalog.skills.push(Skill {
                name: name.to_string(),
                description: "a long routing description that must be shortened".to_string(),
                location: PathBuf::from(format!("/skills/{name}/SKILL.md")),
                base_directory: PathBuf::from(format!("/skills/{name}")),
                scope: SkillScope::Project,
            });
        }
        let budget = SkillCatalogBudget::new(300, 10);
        let build = |enabled: &[String]| {
            build_initial_prompt_messages_with_mcp(
                Path::new("/project"),
                config_dir.path(),
                None,
                None,
                &[],
                &catalog,
                budget,
                SandboxPolicy::WorkspaceWrite,
                &[],
                &[],
                Some(enabled),
                true,
                true,
                true,
            )
        };

        let with_search = render_messages(&build(&["Read".to_string(), "SkillSearch".to_string()]));
        assert!(with_search.contains("<omitted_skills"));
        assert!(with_search.contains("SkillSearch tool"));
        assert!(with_search.contains("- **SkillSearch**:"));

        let without_search = render_messages(&build(&["Read".to_string()]));
        assert!(!without_search.contains("<omitted_skills"));
        assert!(!without_search.contains("SkillSearch tool"));
        assert!(without_search.contains("<name>one</name>"));
        assert!(without_search.contains("<name>three</name>"));
    }

    #[test]
    fn skill_reader_resolution_prefers_read() {
        assert_eq!(SkillReader::resolve(None), Some(SkillReader::Read));
        assert_eq!(
            SkillReader::resolve(Some(&["Bash".to_string(), "Read".to_string()])),
            Some(SkillReader::Read)
        );
        assert_eq!(
            SkillReader::resolve(Some(&["Bash".to_string()])),
            Some(SkillReader::Shell)
        );
        assert_eq!(SkillReader::resolve(Some(&[])), None);
        assert_eq!(SkillReader::resolve(Some(&["Edit".to_string()])), None);
    }

    #[test]
    fn snapshot_empty_prompt() {
        let config_dir = default_config_dir();
        let messages = build_initial_prompt_messages_with_enabled_tools(
            Path::new("/tmp"),
            config_dir.path(),
            None,
            None,
            &[],
            &SkillCatalog::empty(),
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
            true,
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
        let messages = build_initial_prompt_messages_with_enabled_tools(
            Path::new("/project"),
            config_dir.path(),
            None,
            None,
            &files,
            &SkillCatalog::empty(),
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
            true,
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
        let messages = build_initial_prompt_messages_with_enabled_tools(
            Path::new("/project"),
            config_dir.path(),
            None,
            None,
            &files,
            &SkillCatalog::empty(),
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
            true,
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
        let messages = build_initial_prompt_messages_with_enabled_tools(
            Path::new("/project"),
            config_dir.path(),
            None,
            None,
            &[],
            &catalog,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
            true,
        );
        assert_prompt_snapshot("prompt_with_skill_catalog", &messages);
    }

    #[test]
    fn snapshot_with_bash_only_skill_catalog() {
        let config_dir = default_config_dir();
        let mut catalog = SkillCatalog::empty();
        catalog.skills.push(Skill {
            name: "debugging".to_string(),
            description: "How to debug Rust programs".to_string(),
            location: PathBuf::from("/project/.agents/skills/debugging/SKILL.md"),
            base_directory: PathBuf::from("/project/.agents/skills/debugging"),
            scope: SkillScope::Project,
        });
        let enabled = vec!["Bash".to_string()];
        let messages = build_initial_prompt_messages_with_enabled_tools(
            Path::new("/project"),
            config_dir.path(),
            None,
            None,
            &[],
            &catalog,
            SandboxPolicy::WorkspaceWrite,
            &[],
            Some(&enabled),
            true,
            true,
        );
        assert_prompt_snapshot("prompt_with_bash_only_skill_catalog", &messages);
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
        let messages = build_initial_prompt_messages_with_enabled_tools(
            Path::new("/project"),
            config_dir.path(),
            None,
            None,
            &files,
            &catalog,
            SandboxPolicy::WorkspaceWrite,
            &[],
            None,
            true,
            true,
        );
        assert_prompt_snapshot("prompt_with_agents_and_skills", &messages);
    }
}
