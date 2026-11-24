//! Build and compare the sanitized context identity pinned to a session.
//!
//! A preflight identity covers context that is available before toolbox and
//! MCP startup. The full identity runs after those resources are resolved. The
//! comparison classifies a changed working directory, system prompt source,
//! tool catalog, sandbox, or profile definition as a hard mismatch, and content
//! changes in `AGENTS.md`, skills, or the MCP capability set as warnings.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::clients::mcp::McpServer;
use crate::config::settings::LoadedSettings;
use crate::config::{AgentsFile, SkillCatalog};
use crate::prompts::ResolvedSystemPromptSource;
use crate::types::{
    ContextFileIdentity, ContextIdentity, ContextMcpIdentity, ContextMcpServerIdentity,
    ContextProfileIdentity, ContextSkillIdentity, ContextSystemPromptIdentity,
};

/// Current resources used to build one session context identity.
pub struct ContextIdentityInputs<'a> {
    pub working_directory: &'a Path,
    pub config_dir: &'a Path,
    pub include_project_mcp: bool,
    pub system_prompt_source: ResolvedSystemPromptSource,
    pub system_prompt: Option<&'a str>,
    pub agents_files: &'a [AgentsFile],
    pub tool_catalog_digest: &'a str,
    pub skill_catalog: &'a SkillCatalog,
    pub mcp_config_paths: &'a [PathBuf],
    pub mcp_servers: &'a [McpServer],
    pub sandbox: &'a str,
    pub profile_name: Option<&'a str>,
    pub loaded: &'a LoadedSettings,
}

/// Current context available before toolbox and MCP startup.
pub struct ContextPreflightInputs<'a> {
    pub working_directory: &'a Path,
    pub config_dir: &'a Path,
    pub include_project_mcp: bool,
    pub system_prompt_source: ResolvedSystemPromptSource,
    /// Built prompt text when the source is a file; `None` for the built-in
    /// prompt, whose text depends on extension startup.
    pub system_prompt: Option<&'a str>,
    pub agents_files: &'a [AgentsFile],
    pub skill_catalog: &'a SkillCatalog,
    pub mcp_config_paths: &'a [PathBuf],
    pub sandbox: &'a str,
    pub profile_name: Option<&'a str>,
    pub loaded: &'a LoadedSettings,
}

/// How a context difference is handled by the resume path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextDriftSeverity {
    Fail,
    Warn,
    Rebind,
}

/// One field that differs between a stored and a current context identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextDrift {
    pub field: &'static str,
    pub stored: String,
    pub current: String,
    pub severity: ContextDriftSeverity,
}

impl ContextDrift {
    /// Format one stable `field: stored -> current` diagnostic line.
    #[must_use]
    pub fn describe(&self) -> String {
        format!("{}: {} -> {}", self.field, self.stored, self.current)
    }

    /// Whether this difference must stop the resume.
    #[must_use]
    pub const fn is_fail(&self) -> bool {
        matches!(self.severity, ContextDriftSeverity::Fail)
    }

    /// Whether this difference is a warning that strict mode upgrades.
    #[must_use]
    pub const fn is_warn(&self) -> bool {
        matches!(self.severity, ContextDriftSeverity::Warn)
    }

    /// Whether this difference is an explicit working-directory rebind.
    #[must_use]
    pub const fn is_rebind(&self) -> bool {
        matches!(self.severity, ContextDriftSeverity::Rebind)
    }
}

/// Outcome of comparing a stored context identity with the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextIdentityCheck {
    Match,
    /// The session predates context identity pinning.
    UnknownBaseline,
    Drift(Vec<ContextDrift>),
}

/// One-way fingerprint of a full context identity.
///
/// The prompt-prefix metadata stores this instead of the identity itself, so a
/// task-start record and live stream do not repeat paths or per-file digests.
#[must_use]
pub fn fingerprint(identity: &ContextIdentity) -> String {
    serde_json::to_vec(identity).map_or_else(|_| String::new(), |bytes| sha256_hex(&bytes))
}

/// Build the sanitized identity for the current run.
#[must_use]
pub fn build(inputs: ContextIdentityInputs<'_>) -> ContextIdentity {
    let project_root = project_root(inputs.working_directory);
    let system_prompt = ContextSystemPromptIdentity {
        source: inputs.system_prompt_source.source.label().to_string(),
        path: inputs.system_prompt_source.path,
        digest: sha256_hex(inputs.system_prompt.unwrap_or_default().as_bytes()),
    };
    let mcp = mcp_identity(
        inputs.working_directory,
        inputs.config_dir,
        inputs.include_project_mcp,
        inputs.mcp_config_paths,
        inputs.mcp_servers,
    );
    ContextIdentity {
        working_directory: inputs.working_directory.to_path_buf(),
        project_root,
        system_prompt,
        agents_files: file_identities(inputs.agents_files),
        tool_catalog_digest: inputs.tool_catalog_digest.to_string(),
        skills: skill_identities(inputs.skill_catalog),
        mcp,
        sandbox: inputs.sandbox.to_string(),
        profile: profile_identity(inputs.profile_name, inputs.loaded),
    }
}

/// Build the context available before toolbox and MCP startup.
///
/// The tool catalog and MCP server capabilities are left empty because they
/// require extension startup; [`check_preflight`] does not compare them.
#[must_use]
pub fn build_preflight(inputs: ContextPreflightInputs<'_>) -> ContextIdentity {
    let system_prompt = ContextSystemPromptIdentity {
        source: inputs.system_prompt_source.source.label().to_string(),
        path: inputs.system_prompt_source.path,
        digest: inputs
            .system_prompt
            .map_or_else(String::new, |text| sha256_hex(text.as_bytes())),
    };
    ContextIdentity {
        working_directory: inputs.working_directory.to_path_buf(),
        project_root: project_root(inputs.working_directory),
        system_prompt,
        agents_files: file_identities(inputs.agents_files),
        tool_catalog_digest: String::new(),
        skills: skill_identities(inputs.skill_catalog),
        mcp: ContextMcpIdentity {
            config_paths: mcp_config_paths(
                inputs.working_directory,
                inputs.config_dir,
                inputs.include_project_mcp,
                inputs.mcp_config_paths,
            ),
            servers: Vec::new(),
        },
        sandbox: inputs.sandbox.to_string(),
        profile: profile_identity(inputs.profile_name, inputs.loaded),
    }
}

/// Compare a stored context identity with the current rebuild.
#[must_use]
pub fn check(
    stored: Option<&ContextIdentity>,
    current: &ContextIdentity,
    allow_cwd_rebind: bool,
) -> ContextIdentityCheck {
    compare_identity(stored, current, ComparisonScope::Full { allow_cwd_rebind })
}

/// Compare the context that is available before toolbox and MCP startup.
///
/// This omits the tool catalog and MCP server capabilities, which cannot be
/// rebuilt without starting those extensions. A warning that is not strict is
/// suppressed here and reported by the full check after startup.
#[must_use]
pub fn check_preflight(
    stored: Option<&ContextIdentity>,
    current: &ContextIdentity,
    allow_cwd_rebind: bool,
) -> ContextIdentityCheck {
    compare_identity(
        stored,
        current,
        ComparisonScope::Preflight {
            allow_cwd_rebind,
            include_system_prompt_digest: !current.system_prompt.digest.is_empty(),
        },
    )
}

#[derive(Clone, Copy)]
enum ComparisonScope {
    Full {
        allow_cwd_rebind: bool,
    },
    Preflight {
        allow_cwd_rebind: bool,
        include_system_prompt_digest: bool,
    },
}

impl ComparisonScope {
    const fn allow_cwd_rebind(self) -> bool {
        match self {
            Self::Full { allow_cwd_rebind }
            | Self::Preflight {
                allow_cwd_rebind, ..
            } => allow_cwd_rebind,
        }
    }

    const fn includes_tool_catalog(self) -> bool {
        matches!(self, Self::Full { .. })
    }

    const fn includes_mcp_servers(self) -> bool {
        matches!(self, Self::Full { .. })
    }

    const fn includes_system_prompt_digest(self) -> bool {
        match self {
            Self::Full { .. } => true,
            Self::Preflight {
                include_system_prompt_digest,
                ..
            } => include_system_prompt_digest,
        }
    }
}

fn compare_identity(
    stored: Option<&ContextIdentity>,
    current: &ContextIdentity,
    scope: ComparisonScope,
) -> ContextIdentityCheck {
    let Some(stored) = stored else {
        return ContextIdentityCheck::UnknownBaseline;
    };
    let mut differences = Vec::new();
    let path_severity = if scope.allow_cwd_rebind() {
        ContextDriftSeverity::Rebind
    } else {
        ContextDriftSeverity::Fail
    };
    compare_working_directory(
        &mut differences,
        &stored.working_directory,
        &current.working_directory,
        path_severity,
    );
    compare_path(
        &mut differences,
        "project_root",
        &stored.project_root,
        &current.project_root,
        path_severity,
    );
    compare_system_prompt(
        &mut differences,
        &stored.system_prompt,
        &current.system_prompt,
        scope.includes_system_prompt_digest(),
    );
    compare_files(
        &mut differences,
        "agents_files",
        &stored.agents_files,
        &current.agents_files,
    );
    if scope.includes_tool_catalog() {
        compare_text(
            &mut differences,
            "tool_catalog_digest",
            &short_digest(&stored.tool_catalog_digest),
            &short_digest(&current.tool_catalog_digest),
            ContextDriftSeverity::Fail,
        );
    }
    compare_skills(&mut differences, &stored.skills, &current.skills);
    compare_mcp(
        &mut differences,
        &stored.mcp,
        &current.mcp,
        scope.includes_mcp_servers(),
    );
    compare_text(
        &mut differences,
        "sandbox",
        &stored.sandbox,
        &current.sandbox,
        ContextDriftSeverity::Fail,
    );
    compare_profile(&mut differences, &stored.profile, &current.profile);
    if differences.is_empty() {
        ContextIdentityCheck::Match
    } else {
        ContextIdentityCheck::Drift(differences)
    }
}

fn project_root(working_directory: &Path) -> PathBuf {
    std::fs::canonicalize(working_directory).unwrap_or_else(|_| working_directory.to_path_buf())
}

fn file_identities(files: &[AgentsFile]) -> Vec<ContextFileIdentity> {
    files
        .iter()
        .map(|file| ContextFileIdentity {
            path: file.path.clone(),
            digest: sha256_hex(file.content.as_bytes()),
        })
        .collect()
}

fn skill_identities(skill_catalog: &SkillCatalog) -> Vec<ContextSkillIdentity> {
    skill_catalog.skills.iter().map(skill_identity).collect()
}

fn profile_identity(profile_name: Option<&str>, loaded: &LoadedSettings) -> ContextProfileIdentity {
    ContextProfileIdentity {
        name: profile_name.map(str::to_string),
        request_settings_digest: request_settings_digest(loaded),
    }
}

fn skill_identity(skill: &crate::config::skills::Skill) -> ContextSkillIdentity {
    let digest = std::fs::read(&skill.location).map_or_else(
        |_| sha256_hex(skill.description.as_bytes()),
        |bytes| sha256_hex(&bytes),
    );
    ContextSkillIdentity {
        name: skill.name.clone(),
        scope: skill.scope.label().to_string(),
        location: skill.location.clone(),
        digest,
    }
}

fn mcp_identity(
    working_directory: &Path,
    config_dir: &Path,
    include_project_mcp: bool,
    configured_paths: &[PathBuf],
    servers: &[McpServer],
) -> ContextMcpIdentity {
    let config_paths = mcp_config_paths(
        working_directory,
        config_dir,
        include_project_mcp,
        configured_paths,
    );
    let mut servers = servers
        .iter()
        .map(|server| {
            let mut tools = server
                .tools
                .iter()
                .map(|tool| tool.registered_name.clone())
                .collect::<Vec<_>>();
            tools.sort();
            tools.dedup();
            ContextMcpServerIdentity {
                name: server.name.clone(),
                tools,
            }
        })
        .collect::<Vec<_>>();
    servers.sort_by(|left, right| left.name.cmp(&right.name));
    ContextMcpIdentity {
        config_paths,
        servers,
    }
}

fn mcp_config_paths(
    working_directory: &Path,
    config_dir: &Path,
    include_project_mcp: bool,
    configured_paths: &[PathBuf],
) -> Vec<PathBuf> {
    let mut config_paths = Vec::with_capacity(configured_paths.len() + 2);
    config_paths.push(config_dir.join("mcp.json"));
    if include_project_mcp {
        config_paths.push(working_directory.join(".cake").join("mcp.json"));
    }
    config_paths.extend(configured_paths.iter().cloned());
    config_paths.sort();
    config_paths.dedup();
    config_paths
}

fn request_settings_digest(loaded: &LoadedSettings) -> String {
    let value = serde_json::json!({
        "tools_enabled": &loaded.tools_enabled,
        "skills": &loaded.skills,
        "limits": &loaded.limits,
        "images": &loaded.images,
        "compaction": &loaded.compaction,
        "compaction_summarizer": &loaded.compaction_summarizer,
    });
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    sha256_hex(&bytes)
}

fn compare_working_directory(
    differences: &mut Vec<ContextDrift>,
    stored: &Path,
    current: &Path,
    severity: ContextDriftSeverity,
) {
    if canonical_or_self(stored) != canonical_or_self(current) {
        differences.push(ContextDrift {
            field: "working_directory",
            stored: stored.display().to_string(),
            current: current.display().to_string(),
            severity,
        });
    }
}

fn compare_path(
    differences: &mut Vec<ContextDrift>,
    field: &'static str,
    stored: &Path,
    current: &Path,
    severity: ContextDriftSeverity,
) {
    if stored != current {
        differences.push(ContextDrift {
            field,
            stored: stored.display().to_string(),
            current: current.display().to_string(),
            severity,
        });
    }
}

fn canonical_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn compare_system_prompt(
    differences: &mut Vec<ContextDrift>,
    stored: &ContextSystemPromptIdentity,
    current: &ContextSystemPromptIdentity,
    include_digest: bool,
) {
    compare_text(
        differences,
        "system_prompt.source",
        &stored.source,
        &current.source,
        ContextDriftSeverity::Fail,
    );
    compare_text(
        differences,
        "system_prompt.path",
        &show_path(stored.path.as_deref()),
        &show_path(current.path.as_deref()),
        ContextDriftSeverity::Fail,
    );
    if include_digest {
        compare_text(
            differences,
            "system_prompt.digest",
            &short_digest(&stored.digest),
            &short_digest(&current.digest),
            ContextDriftSeverity::Warn,
        );
    }
}

fn compare_files(
    differences: &mut Vec<ContextDrift>,
    field: &'static str,
    stored: &[ContextFileIdentity],
    current: &[ContextFileIdentity],
) {
    let stored = describe_files(stored);
    let current = describe_files(current);
    compare_text(
        differences,
        field,
        &stored,
        &current,
        ContextDriftSeverity::Warn,
    );
}

fn compare_skills(
    differences: &mut Vec<ContextDrift>,
    stored: &[ContextSkillIdentity],
    current: &[ContextSkillIdentity],
) {
    let stored = describe_skills(stored);
    let current = describe_skills(current);
    compare_text(
        differences,
        "skills",
        &stored,
        &current,
        ContextDriftSeverity::Warn,
    );
}

fn compare_mcp(
    differences: &mut Vec<ContextDrift>,
    stored: &ContextMcpIdentity,
    current: &ContextMcpIdentity,
    include_servers: bool,
) {
    compare_text(
        differences,
        "mcp.config_paths",
        &describe_paths(&stored.config_paths),
        &describe_paths(&current.config_paths),
        ContextDriftSeverity::Warn,
    );
    if include_servers {
        compare_text(
            differences,
            "mcp.servers",
            &describe_servers(&stored.servers),
            &describe_servers(&current.servers),
            ContextDriftSeverity::Warn,
        );
    }
}

fn compare_profile(
    differences: &mut Vec<ContextDrift>,
    stored: &ContextProfileIdentity,
    current: &ContextProfileIdentity,
) {
    compare_text(
        differences,
        "profile.name",
        stored.name.as_deref().unwrap_or("none"),
        current.name.as_deref().unwrap_or("none"),
        ContextDriftSeverity::Fail,
    );
    compare_text(
        differences,
        "profile.request_settings_digest",
        &short_digest(&stored.request_settings_digest),
        &short_digest(&current.request_settings_digest),
        ContextDriftSeverity::Fail,
    );
}

fn compare_text(
    differences: &mut Vec<ContextDrift>,
    field: &'static str,
    stored: &str,
    current: &str,
    severity: ContextDriftSeverity,
) {
    if stored != current {
        differences.push(ContextDrift {
            field,
            stored: stored.to_string(),
            current: current.to_string(),
            severity,
        });
    }
}

fn show_path(path: Option<&Path>) -> String {
    path.map_or_else(|| "none".to_string(), |path| path.display().to_string())
}

fn describe_files(files: &[ContextFileIdentity]) -> String {
    if files.is_empty() {
        return "none".to_string();
    }
    files
        .iter()
        .map(|file| format!("{}:{}", file.path, short_digest(&file.digest)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_skills(skills: &[ContextSkillIdentity]) -> String {
    if skills.is_empty() {
        return "none".to_string();
    }
    skills
        .iter()
        .map(|skill| {
            format!(
                "{}@{}:{}",
                skill.name,
                skill.scope,
                short_digest(&skill.digest)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "none".to_string();
    }
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_servers(servers: &[ContextMcpServerIdentity]) -> String {
    if servers.is_empty() {
        return "none".to_string();
    }
    servers
        .iter()
        .map(|server| format!("{}({})", server.name, server.tools.join(",")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn short_digest(digest: &str) -> String {
    digest
        .get(..12)
        .map_or_else(|| digest.to_string(), |prefix| format!("{prefix}..."))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
