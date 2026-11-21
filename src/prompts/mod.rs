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
/// Resolve the system prompt, optionally skipping automatic project content.
///
/// `include_project_prompt` is false under `--no-project-config`. The automatic
/// `.cake/system.md` source is also skipped when it is a symlink, even in a
/// trusted project, so project prompt content cannot read a host file through a
/// link.
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
    if let Some(path) = cli_system_prompt
        && let Some(prompt) = read_prompt_override(path, PromptSource::Cli)
    {
        return prompt;
    }

    if include_project_prompt && let Some(prompt) = read_project_prompt(working_dir) {
        return prompt;
    }

    if let Some(path) = settings_system_prompt
        && let Some(prompt) = read_prompt_override(path, PromptSource::Settings)
    {
        return prompt;
    }

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
/// Build initial prompt messages, optionally skipping automatic project content.
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
