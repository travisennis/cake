//! `cake debug config`: the effective runtime configuration and trust state.
//!
//! The report explains what a run in this directory would resolve: settings
//! precedence, the selected profile and model (with credential presence, never
//! the credential value), the system prompt source, the sandbox policy and its
//! effective grants, the selected tools and skills, hooks, and toolbox
//! discovery. It is read-only: it does not resolve a provider credential, start
//! a hook, gate, MCP server, or toolbox process, create a configured directory,
//! or open a session. The grants section is the same [`crate::clients::GrantReport`]
//! value `debug grants` renders.

use crate::cli::debug::api_type_label;
use crate::cli::debug::grants;
use crate::cli::debug::print_project_trust_warning;
use crate::cli::debug::project_trust_warning_message;
use crate::cli::debug::skills_report;
use crate::cli::settings::load_settings;
use crate::cli::{
    CHECK_CONFIG_HOOKS, CHECK_CONFIG_MODEL, CHECK_CONFIG_PROJECT_TRUST, CHECK_GRANTS_PATH,
    CHECK_SKILLS_DISCOVERY, CommandRunOptions, DiagnosticCheck, DiagnosticDocument,
    settings_warnings,
};
use crate::clients::{GrantReport, SandboxPolicy, effective_builtin_tool_names};
use crate::config::hooks::{HookGroup, HookMatcher, HooksLoader};
use crate::config::settings::{JudgeSettings, LoadedSettings, ResolvedLimits};
use crate::config::skills::{
    SkillCatalog, SkillConfig, SkillDiagnostic, discover_skills_with_paths, parse_skill_path_list,
};
use crate::config::toolbox::{
    TOOLBOX_ENV_VAR, discover_toolbox_entries, filter_project_toolbox_dirs, toolbox_directories,
};
use crate::config::trust::{HookFileSource, ProjectConfigMode, ProjectTrustReport};
use crate::config::{ModelDefinition, SettingsLoader};
use crate::prompts::{ResolvedSystemPromptSource, resolve_system_prompt_source};
use serde::Serialize;
use serde_json::json;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// One invocation's local report overrides.
pub(super) struct ConfigInputs<'a> {
    pub sandbox: Option<SandboxPolicy>,
    pub add_dir: &'a [String],
    pub no_skills: bool,
    pub skills: Option<&'a str>,
    pub no_tools: bool,
    pub tools: Option<&'a str>,
    pub toolbox: &'a [PathBuf],
}

/// Inspect the effective runtime configuration and project trust.
pub(super) fn run_config(
    inputs: &ConfigInputs<'_>,
    json: bool,
    data_dir: &crate::config::DataDir,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    let load = load_settings("debug config", json, options)?;
    if !json {
        load.settings.print_warnings();
        print_project_trust_warning(&load.trust);
    }

    let report = ConfigReport::build(
        &load.settings,
        &load.trust,
        &load.hook_files,
        data_dir,
        inputs,
        options,
    )?;

    if json {
        print!("{}", report.render_json()?);
    } else {
        print!("{}", report.format_text());
    }
    Ok(())
}

/// One resolved model row in the report.
#[derive(Debug, Serialize)]
struct ModelReport {
    /// The `[[models]]` entry name a run would select, or `None`.
    name: Option<String>,
    /// `--model`, `default_model`, or `None`.
    source: Option<&'static str>,
    /// Whether the selected name exists in the loaded model set.
    configured: bool,
    provider_model: Option<String>,
    api_type: Option<&'static str>,
    base_url: Option<String>,
    supports_images: bool,
    /// Credential source and presence. The value is never read into the report.
    credential: Option<CredentialReport>,
}

#[derive(Debug, Serialize)]
struct CredentialReport {
    /// `environment` or `codex_auth_file`.
    source: &'static str,
    /// Environment variable name for an environment-backed model.
    name: Option<String>,
    /// Whether the credential appears to be available.
    present: bool,
}

impl ModelReport {
    fn build(loaded: &LoadedSettings, options: &CommandRunOptions<'_>) -> Self {
        let (name, source) = options.model.map_or_else(
            || {
                loaded
                    .default_model
                    .as_deref()
                    .map_or((None, None), |default| {
                        (Some(default.to_string()), Some("default_model"))
                    })
            },
            |model| (Some(model.to_string()), Some("--model")),
        );
        let definition = name.as_deref().and_then(|name| loaded.models.get(name));
        let credential = definition.map(credential_report);
        Self {
            name,
            source,
            configured: definition.is_some(),
            provider_model: definition.map(|definition| definition.model.clone()),
            api_type: definition.map(|definition| api_type_label(definition.api_type)),
            base_url: definition.map(|definition| definition.base_url.clone()),
            supports_images: definition.is_some_and(|definition| definition.supports_images),
            credential,
        }
    }
}

fn credential_report(definition: &ModelDefinition) -> CredentialReport {
    if crate::auth::is_chatgpt_codex_backend(&definition.base_url) {
        let present = crate::auth::codex_auth_path().is_ok_and(|path| path.is_file());
        CredentialReport {
            source: "codex_auth_file",
            name: None,
            present,
        }
    } else {
        let present = std::env::var(&definition.api_key_env).is_ok_and(|value| !value.is_empty());
        CredentialReport {
            source: "environment",
            name: Some(definition.api_key_env.clone()),
            present,
        }
    }
}

/// One settings file and whether the run reads and applies it.
#[derive(Debug, Serialize)]
struct SettingsSourceReport {
    origin: &'static str,
    path: Option<String>,
    present: bool,
    read: bool,
    authority_applied: bool,
}

impl SettingsSourceReport {
    fn build(trust: &ProjectTrustReport, mode: ProjectConfigMode, config_dir: &Path) -> Vec<Self> {
        let global = config_dir.join("settings.toml");
        let project = trust.project_root.join(".cake").join("settings.toml");
        let reads_project = mode.loads_project_config();
        let project_authority = reads_project && (mode.is_enforcing() || trust.allows_authority());
        vec![
            Self {
                origin: "global",
                path: Some(global.display().to_string()),
                present: global.is_file(),
                read: true,
                authority_applied: true,
            },
            Self {
                origin: "project",
                path: reads_project.then(|| project.display().to_string()),
                present: reads_project && project.is_file(),
                read: reads_project,
                authority_applied: project_authority,
            },
        ]
    }
}

/// Resolved values a run consumes after every settings layer is merged.
#[derive(Debug, Serialize)]
struct SettingsSnapshot {
    directories: Vec<String>,
    sandbox: SandboxSnapshot,
    skills: SkillSettingsSnapshot,
    tools_enabled: Option<Vec<String>>,
    limits: LimitsSnapshot,
    judge: JudgeSnapshot,
}

#[derive(Debug, Serialize)]
struct SandboxSnapshot {
    read_only: Vec<String>,
    writable: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SkillSettingsSnapshot {
    disabled: bool,
    only: Vec<String>,
    path: Option<String>,
    resolved_paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct LimitsSnapshot {
    max_turns: Option<u32>,
    max_tool_calls: Option<u32>,
    bash_output_max_bytes: Option<usize>,
    bash_read_cap: Option<usize>,
    bash_session_output_max_bytes: usize,
    bash_session_max: usize,
    bash_session_max_seconds: u64,
    bash_session_exited_ttl_seconds: u64,
    read_default_end_line: Option<usize>,
    read_max_output_bytes: Option<usize>,
    edit_max_file_bytes: Option<usize>,
    read_max_line_bytes: Option<usize>,
    read_image_max_bytes: Option<usize>,
    hook_output_limit: Option<usize>,
}

impl From<&ResolvedLimits> for LimitsSnapshot {
    fn from(limits: &ResolvedLimits) -> Self {
        let tools = limits.tool_limits;
        Self {
            max_turns: limits.max_turns,
            max_tool_calls: limits.max_tool_calls,
            bash_output_max_bytes: tools.bash_output_max_bytes,
            bash_read_cap: tools.bash_read_cap,
            bash_session_output_max_bytes: tools.bash_session_output_max_bytes,
            bash_session_max: tools.bash_session_max,
            bash_session_max_seconds: tools.bash_session_max_seconds,
            bash_session_exited_ttl_seconds: tools.bash_session_exited_ttl_seconds,
            read_default_end_line: tools.read_default_end_line,
            read_max_output_bytes: tools.read_max_output_bytes,
            edit_max_file_bytes: tools.edit_max_file_bytes,
            read_max_line_bytes: tools.read_max_line_bytes,
            read_image_max_bytes: tools.read_image_max_bytes,
            hook_output_limit: tools.hook_output_limit,
        }
    }
}

#[derive(Debug, Serialize)]
struct JudgeSnapshot {
    enabled: bool,
    model: Option<String>,
    timeout_secs: u64,
    retry_budget_secs: u64,
    rubric_file: Option<String>,
}

impl From<&JudgeSettings> for JudgeSnapshot {
    fn from(judge: &JudgeSettings) -> Self {
        Self {
            enabled: judge.enabled,
            model: judge.model.clone(),
            timeout_secs: judge.timeout_secs,
            retry_budget_secs: judge.retry_budget_secs,
            rubric_file: judge
                .rubric_file
                .as_ref()
                .map(|path| path.display().to_string()),
        }
    }
}

impl SettingsSnapshot {
    fn build(loaded: &LoadedSettings, roots: &[PathBuf]) -> Self {
        Self {
            directories: loaded.directories.clone(),
            sandbox: SandboxSnapshot {
                read_only: loaded.sandbox.read_only.clone(),
                writable: loaded.sandbox.writable.clone(),
            },
            skills: SkillSettingsSnapshot {
                disabled: loaded.skills.disabled,
                only: loaded.skills.only.clone(),
                path: loaded.skills.path.clone(),
                resolved_paths: path_strings(roots),
            },
            tools_enabled: loaded.tools_enabled.clone(),
            limits: LimitsSnapshot::from(&loaded.limits),
            judge: JudgeSnapshot::from(&loaded.judge),
        }
    }
}

/// The selected system prompt source, without its text.
#[derive(Debug, Serialize)]
struct PromptReport {
    source: &'static str,
    path: Option<String>,
    configured_path: Option<String>,
}

impl PromptReport {
    fn build(
        loaded: &LoadedSettings,
        mode: ProjectConfigMode,
        working_dir: &Path,
        config_dir: &Path,
    ) -> Self {
        let ResolvedSystemPromptSource { source, path } = resolve_system_prompt_source(
            working_dir,
            config_dir,
            None,
            loaded.system_prompt.as_deref().map(Path::new),
            mode.loads_project_config(),
        );
        Self {
            source: source.label(),
            path: path.map(|path| path.display().to_string()),
            configured_path: loaded.system_prompt.clone(),
        }
    }
}

/// The effective tool selection and the built-in registry subset.
#[derive(Debug, Serialize)]
struct ToolsReport {
    /// `--tools`/`--no-tools`, `settings`, or `default`.
    source: &'static str,
    /// Exact-name selection, `None` for all tools.
    selected: Option<Vec<String>>,
    /// Built-in names after model, sandbox, and selection filters.
    builtin: Vec<String>,
}

impl ToolsReport {
    fn build(
        loaded: &LoadedSettings,
        inputs: &ConfigInputs<'_>,
        model: &ModelReport,
        policy: SandboxPolicy,
    ) -> Self {
        let selected = SettingsLoader::resolve_tools_config(
            inputs.no_tools,
            inputs.tools,
            loaded.tools_enabled.as_deref(),
        );
        let source = if inputs.no_tools {
            "--no-tools"
        } else if inputs.tools.is_some() {
            "--tools"
        } else if loaded.tools_enabled.is_some() {
            "settings"
        } else {
            "default"
        };
        let builtin =
            effective_builtin_tool_names(policy, model.supports_images, selected.as_deref());
        Self {
            source,
            selected,
            builtin,
        }
    }
}

/// One skill row in the effective-configuration report.
#[derive(Debug, Serialize)]
struct SkillRow {
    name: String,
    scope: &'static str,
    location: String,
    base_directory: String,
}

fn skill_rows(catalog: &SkillCatalog) -> Vec<SkillRow> {
    let mut rows: Vec<SkillRow> = catalog
        .skills
        .iter()
        .map(|skill| SkillRow {
            name: skill.name.clone(),
            scope: skill.scope.label(),
            location: skill.location.display().to_string(),
            base_directory: skill.base_directory.display().to_string(),
        })
        .collect();
    rows.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.location.cmp(&right.location))
    });
    rows
}

/// The selected and filtered skill sets plus their diagnostics.
#[derive(Debug, Serialize)]
struct SkillsReport {
    /// `all`, `disabled`, or `only`.
    selection: &'static str,
    /// Where the selection came from: `--no-skills`, `--skills`, `settings`, or `default`.
    reason: &'static str,
    selected: Vec<SkillRow>,
    filtered: Vec<SkillRow>,
    diagnostics: Vec<SkillDiagnostic>,
}

impl SkillsReport {
    fn build(
        discovered: &SkillCatalog,
        selected: &SkillCatalog,
        selection: &SkillConfig,
        inputs: &ConfigInputs<'_>,
    ) -> Self {
        let (label, reason) = match selection {
            SkillConfig::Disabled => (
                "disabled",
                if inputs.no_skills {
                    "--no-skills"
                } else if inputs.skills.is_some() {
                    "--skills"
                } else {
                    "settings"
                },
            ),
            SkillConfig::Only(_) => (
                "only",
                if inputs.skills.is_some() {
                    "--skills"
                } else {
                    "settings"
                },
            ),
            SkillConfig::All => ("all", "default"),
        };
        let selected_rows = skill_rows(selected);
        let filtered = discovered
            .skills
            .iter()
            .filter(|skill| {
                !selected
                    .skills
                    .iter()
                    .any(|selected| selected.location == skill.location)
            })
            .map(|skill| SkillRow {
                name: skill.name.clone(),
                scope: skill.scope.label(),
                location: skill.location.display().to_string(),
                base_directory: skill.base_directory.display().to_string(),
            })
            .collect::<Vec<_>>();
        let mut filtered = filtered;
        filtered.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.location.cmp(&right.location))
        });
        let mut diagnostics = discovered.diagnostics.clone();
        for diagnostic in &selected.diagnostics {
            if !diagnostics.contains(diagnostic) {
                diagnostics.push(diagnostic.clone());
            }
        }
        diagnostics.sort_by(|left, right| {
            left.file
                .cmp(&right.file)
                .then_with(|| left.level.label().cmp(right.level.label()))
                .then_with(|| left.message.cmp(&right.message))
        });
        diagnostics.dedup();
        Self {
            selection: label,
            reason,
            selected: selected_rows,
            filtered,
            diagnostics,
        }
    }
}

/// One hook file and whether this report loaded it.
#[derive(Debug, Serialize)]
struct HookSourceReport {
    origin: &'static str,
    path: String,
    /// `loaded`, `absent`, `invalid`, `untrusted`, or `not-read`.
    status: &'static str,
    group_count: usize,
    error: Option<String>,
}

/// One loaded hook group, with the command text omitted.
#[derive(Debug, Serialize)]
struct HookGroupReport {
    source: String,
    event: &'static str,
    matcher: String,
    hook_count: usize,
    fail_closed_count: usize,
}

#[derive(Debug, Serialize)]
struct HooksReport {
    sources: Vec<HookSourceReport>,
    groups: Vec<HookGroupReport>,
    commands_redacted: bool,
}

impl HooksReport {
    fn build(
        hook_files: &[HookFileSource],
        trust: &ProjectTrustReport,
        mode: ProjectConfigMode,
        config_dir: &Path,
    ) -> Self {
        let global_path = config_dir.join("hooks.json");
        let mut sources = Vec::new();
        let mut groups = Vec::new();

        let (status, error) = if global_path.is_file() {
            match HooksLoader::load_with_project_sources(Some(&global_path), &[]) {
                Ok(loaded) => {
                    append_groups(
                        &mut groups,
                        &global_path.display().to_string(),
                        &loaded.groups,
                    );
                    ("loaded", None)
                },
                Err(error) => ("invalid", Some(error.to_string())),
            }
        } else {
            ("absent", None)
        };
        sources.push(HookSourceReport {
            origin: "global",
            path: global_path.display().to_string(),
            status,
            group_count: group_count(&groups, &global_path.display().to_string()),
            error,
        });

        let mut project_files: Vec<&HookFileSource> = hook_files.iter().collect();
        project_files.sort_by(|left, right| left.path.cmp(&right.path));
        for source in project_files {
            let (status, error) = if !mode.loads_project_config() {
                ("not-read", None)
            } else if !trust.allows_authority() {
                ("untrusted", None)
            } else {
                match HooksLoader::load_with_project_sources(None, std::slice::from_ref(source)) {
                    Ok(loaded) => {
                        append_groups(
                            &mut groups,
                            &source.path.display().to_string(),
                            &loaded.groups,
                        );
                        ("loaded", None)
                    },
                    Err(error) => ("invalid", Some(error.to_string())),
                }
            };
            sources.push(HookSourceReport {
                origin: "project",
                path: source.path.display().to_string(),
                status,
                group_count: group_count(&groups, &source.path.display().to_string()),
                error,
            });
        }

        groups.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.event.cmp(right.event))
                .then_with(|| left.matcher.cmp(&right.matcher))
        });
        Self {
            sources,
            groups,
            commands_redacted: true,
        }
    }
}

fn append_groups(output: &mut Vec<HookGroupReport>, source: &str, groups: &[HookGroup]) {
    for group in groups {
        output.push(HookGroupReport {
            source: source.to_string(),
            event: group.event.as_str(),
            matcher: matcher_label(&group.matcher),
            hook_count: group.hooks.len(),
            fail_closed_count: group.hooks.iter().filter(|hook| hook.fail_closed).count(),
        });
    }
}

fn matcher_label(matcher: &HookMatcher) -> String {
    match matcher {
        HookMatcher::All => "*".to_string(),
        HookMatcher::Exact(values) => values
            .iter()
            .filter_map(|value| value.as_display_str())
            .collect::<Vec<_>>()
            .join("|"),
    }
}

fn group_count(groups: &[HookGroupReport], source: &str) -> usize {
    groups.iter().filter(|group| group.source == source).count()
}

/// One executable candidate in a toolbox directory, before `describe` runs.
#[derive(Debug, Serialize)]
struct ToolboxCandidateReport {
    filename: String,
    path: String,
}

/// One toolbox directory and its unprobed candidates.
#[derive(Debug, Serialize)]
struct ToolboxDirectoryReport {
    path: String,
    project_local: bool,
    trusted: bool,
    /// `loadable`, `untrusted`, or `read-only`.
    status: &'static str,
    exists: bool,
    candidates: Vec<ToolboxCandidateReport>,
}

#[derive(Debug, Serialize)]
struct ToolboxReport {
    describe_probed: bool,
    directories: Vec<ToolboxDirectoryReport>,
}

impl ToolboxReport {
    fn build(
        extra_dirs: &[PathBuf],
        trust: &ProjectTrustReport,
        mode: ProjectConfigMode,
        config_dir: &Path,
        policy: SandboxPolicy,
        working_dir: &Path,
    ) -> Self {
        let mut dirs = toolbox_directories(
            std::env::var(TOOLBOX_ENV_VAR).ok().as_deref(),
            extra_dirs,
            &config_dir.join("tools"),
            working_dir,
            working_dir,
        );
        if !mode.loads_project_config() {
            dirs = filter_project_toolbox_dirs(dirs, &[working_dir]);
        }

        let project_cake_root = trust.project_root.join(".cake");
        let directories = dirs
            .into_iter()
            .map(|dir| {
                let project_local = dir.starts_with(&project_cake_root);
                let trusted = !project_local || trust.allows_authority();
                let status = if !trusted {
                    "untrusted"
                } else if policy == SandboxPolicy::ReadOnly {
                    "read-only"
                } else {
                    "loadable"
                };
                let candidates = discover_toolbox_entries(std::slice::from_ref(&dir))
                    .into_iter()
                    .map(|entry| ToolboxCandidateReport {
                        filename: entry.filename,
                        path: entry.path.display().to_string(),
                    })
                    .collect();
                ToolboxDirectoryReport {
                    path: dir.display().to_string(),
                    project_local,
                    trusted,
                    status,
                    exists: dir.is_dir(),
                    candidates,
                }
            })
            .collect();
        Self {
            describe_probed: false,
            directories,
        }
    }
}

/// The complete read-only configuration report.
pub(super) struct ConfigReport {
    working_dir: PathBuf,
    profile: Option<String>,
    model: ModelReport,
    settings_sources: Vec<SettingsSourceReport>,
    settings: SettingsSnapshot,
    settings_warnings: Vec<String>,
    trust: ProjectTrustReport,
    prompt: PromptReport,
    grants: GrantReport,
    tools: ToolsReport,
    skills: SkillsReport,
    hooks: HooksReport,
    toolbox: ToolboxReport,
}

impl ConfigReport {
    pub(super) fn build(
        loaded: &LoadedSettings,
        trust: &ProjectTrustReport,
        hook_files: &[HookFileSource],
        data_dir: &crate::config::DataDir,
        inputs: &ConfigInputs<'_>,
        options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<Self> {
        let working_dir = std::env::current_dir()
            .map_err(|error| anyhow::anyhow!("Failed to get current directory: {error}"))?;
        let config_dir = crate::config::config_dir().join("cake");
        let roots = loaded
            .skills
            .path
            .as_deref()
            .map(parse_skill_path_list)
            .unwrap_or_default();
        let discovered = discover_skills_with_paths(&working_dir, &roots);
        let selection =
            SettingsLoader::resolve_skill_config(inputs.no_skills, inputs.skills, &loaded.skills);
        let selected = selection.apply(discovered.clone());
        let skill_dirs = selected
            .skills
            .iter()
            .map(|skill| skill.base_directory.clone())
            .collect();
        let grants = grants::build_report(
            inputs.sandbox,
            inputs.add_dir,
            loaded,
            skill_dirs,
            &working_dir,
            data_dir,
        );
        let model = ModelReport::build(loaded, options);
        let tools = ToolsReport::build(loaded, inputs, &model, grants.policy);
        let settings_sources =
            SettingsSourceReport::build(trust, options.project_config_mode, &config_dir);
        let settings = SettingsSnapshot::build(loaded, &roots);
        let prompt = PromptReport::build(
            loaded,
            options.project_config_mode,
            &working_dir,
            &config_dir,
        );
        let skills = SkillsReport::build(&discovered, &selected, &selection, inputs);
        let hooks = HooksReport::build(hook_files, trust, options.project_config_mode, &config_dir);
        let toolbox = ToolboxReport::build(
            inputs.toolbox,
            trust,
            options.project_config_mode,
            &config_dir,
            grants.policy,
            &working_dir,
        );

        Ok(Self {
            working_dir,
            profile: options.profile.map(str::to_string),
            model,
            settings_sources,
            settings,
            settings_warnings: loaded.warnings.clone(),
            trust: trust.clone(),
            prompt,
            grants,
            tools,
            skills,
            hooks,
            toolbox,
        })
    }

    fn all_checks(&self) -> Vec<DiagnosticCheck> {
        let mut checks = settings_warnings(&self.settings_warnings);
        if let Some(message) = project_trust_warning_message(&self.trust) {
            checks.push(DiagnosticCheck::warning(
                CHECK_CONFIG_PROJECT_TRUST,
                message,
            ));
        }
        checks.extend(self.report_checks());
        checks
    }

    /// A compact, shareable context for another diagnostic command.
    ///
    /// This deliberately omits the configured endpoint, credential variable
    /// name, prompt path and body, hook text, and every full grant path list.
    pub(super) fn session_context(&self) -> serde_json::Value {
        json!({
            "working_directory": self.working_dir.display().to_string(),
            "profile": self.profile,
            "model": {
                "name": self.model.name,
                "source": self.model.source,
                "configured": self.model.configured,
                "provider_model": self.model.provider_model,
                "api_type": self.model.api_type,
                "credential_source": self.model.credential.as_ref().map(|credential| credential.source),
                "credential_present": self.model.credential.as_ref().map(|credential| credential.present),
            },
            "project_trust": {
                "state": self.trust.state.label(),
                "mode": self.trust.mode.label(),
                "one_run": self.trust.mode.one_run(),
            },
            "grants": grants::summary(&self.grants),
            "tools": {
                "source": self.tools.source,
                "selected": self.tools.selected,
                "builtin_count": self.tools.builtin.len(),
            },
            "skills": {
                "selected_count": self.skills.selected.len(),
                "filtered_count": self.skills.filtered.len(),
            },
        })
    }

    /// Findings that matter when the configuration report is context for
    /// another command: settings warnings, project trust, and an unavailable
    /// selected model. Skill, grant, hook, and toolbox details stay with the
    /// commands that own them.
    pub(super) fn session_checks(&self) -> Vec<DiagnosticCheck> {
        let mut checks = settings_warnings(&self.settings_warnings);
        if let Some(message) = project_trust_warning_message(&self.trust) {
            checks.push(DiagnosticCheck::warning(
                CHECK_CONFIG_PROJECT_TRUST,
                message,
            ));
        }
        if let Some(name) = &self.model.name
            && !self.model.configured
        {
            checks.push(DiagnosticCheck::warning(
                CHECK_CONFIG_MODEL,
                format!("selected model '{name}' is not configured in settings.toml"),
            ));
        }
        checks
    }

    /// Findings the report itself produced, excluding settings and trust
    /// findings that text mode already writes to stderr.
    fn report_checks(&self) -> Vec<DiagnosticCheck> {
        let mut checks = Vec::new();
        if let Some(name) = &self.model.name
            && !self.model.configured
        {
            checks.push(DiagnosticCheck::warning(
                CHECK_CONFIG_MODEL,
                format!("selected model '{name}' is not configured in settings.toml"),
            ));
        }
        checks.extend(self.skills.diagnostics.iter().map(|diagnostic| {
            DiagnosticCheck::warning(
                CHECK_SKILLS_DISCOVERY,
                skills_report::format_skill_diagnostic(diagnostic),
            )
        }));
        checks.extend(
            self.grants
                .warnings()
                .into_iter()
                .map(|warning| DiagnosticCheck::warning(CHECK_GRANTS_PATH, warning)),
        );
        checks.extend(self.hooks.sources.iter().filter_map(|source| {
            source.error.as_ref().map(|error| {
                DiagnosticCheck::warning(
                    CHECK_CONFIG_HOOKS,
                    format!("hook file '{}' could not be loaded: {error}", source.path),
                )
            })
        }));
        checks
    }

    fn render_json(&self) -> anyhow::Result<String> {
        let checks = self.all_checks();
        DiagnosticDocument::new(
            "debug config",
            json!({
                "profile": self.profile,
                "model": self.model.name,
                "model_configured": self.model.configured,
                "credential_present": self.model.credential.as_ref().map(|credential| credential.present),
                "policy": self.grants.policy.label(),
                "policy_source": self.grants.policy_source.label(),
                "sandbox_enforced": self.grants.policy.applies_os_sandbox(),
                "settings_source_count": self.settings_sources.len(),
                "skill_count": self.skills.selected.len(),
                "filtered_skill_count": self.skills.filtered.len(),
                "hook_source_count": self.hooks.sources.len(),
                "hook_group_count": self.hooks.groups.len(),
                "toolbox_directory_count": self.toolbox.directories.len(),
                "toolbox_candidate_count": self
                    .toolbox
                    .directories
                    .iter()
                    .map(|directory| directory.candidates.len())
                    .sum::<usize>(),
                "builtin_tool_count": self.tools.builtin.len(),
                "warning_count": checks.len(),
            }),
            checks,
            json!({
                "working_directory": self.working_dir.display().to_string(),
                "profile": {
                    "name": self.profile,
                    "source": self.profile.as_ref().map(|_| "--profile"),
                },
                "model": self.model,
                "settings_sources": self.settings_sources,
                "settings": self.settings,
                "system_prompt": self.prompt,
                "project_trust": {
                    "state": self.trust.state.label(),
                    "mode": self.trust.mode.label(),
                    "one_run": self.trust.mode.one_run(),
                    "authority": self.trust.authority,
                    "changes": self.trust.changes,
                },
                "grants": grants::flat_payload(&self.grants),
                "tools": self.tools,
                "skills": self.skills,
                "hooks": self.hooks,
                "toolbox": self.toolbox,
            }),
        )
        .render()
    }

    fn format_text(&self) -> String {
        let mut output = String::from("Effective configuration\n");
        _ = writeln!(output, "Working directory: {}", self.working_dir.display());
        match &self.profile {
            Some(profile) => _ = writeln!(output, "Profile: {profile} (from --profile)"),
            None => output.push_str("Profile: none\n"),
        }
        self.append_model(&mut output);
        self.append_settings_sources(&mut output);
        self.append_settings(&mut output);
        self.append_trust(&mut output);
        self.append_prompt(&mut output);
        self.append_grants(&mut output);
        self.append_tools(&mut output);
        self.append_skills(&mut output);
        self.append_hooks(&mut output);
        self.append_toolbox(&mut output);
        self.append_diagnostics(&mut output);
        output
    }

    fn append_model(&self, output: &mut String) {
        output.push_str("Model\n");
        let name = self.model.name.as_deref().unwrap_or("(none)");
        match self.model.source {
            Some(source) => _ = writeln!(output, "  Selected: {name} (from {source})"),
            None => _ = writeln!(output, "  Selected: {name}"),
        }
        if !self.model.configured {
            output.push_str("  Configured: no\n");
            return;
        }
        _ = writeln!(
            output,
            "  Provider model: {}",
            self.model.provider_model.as_deref().unwrap_or("(unknown)")
        );
        _ = writeln!(
            output,
            "  API: {}",
            self.model.api_type.unwrap_or("(unknown)")
        );
        _ = writeln!(
            output,
            "  Endpoint: {}",
            self.model.base_url.as_deref().unwrap_or("(unknown)")
        );
        if let Some(credential) = &self.model.credential {
            let present = if credential.present {
                "present"
            } else {
                "missing"
            };
            match &credential.name {
                Some(name) => _ = writeln!(output, "  Credential: environment {name} ({present})"),
                None => _ = writeln!(output, "  Credential: {} ({present})", credential.source),
            }
        }
    }

    fn append_settings_sources(&self, output: &mut String) {
        output.push_str("Settings sources\n");
        for source in &self.settings_sources {
            let path = source.path.as_deref().unwrap_or("(not read)");
            let mut state = if !source.read {
                "not read".to_string()
            } else if source.present {
                "present".to_string()
            } else {
                "absent".to_string()
            };
            if source.read && source.present {
                state.push_str(if source.authority_applied {
                    ", authority applied"
                } else {
                    ", low-authority only"
                });
            }
            _ = writeln!(output, "  {} {path} ({state})", source.origin);
        }
    }

    fn append_settings(&self, output: &mut String) {
        output.push_str("Settings\n");
        append_string_list(output, "directories", &self.settings.directories);
        append_string_list(
            output,
            "sandbox.read_only",
            &self.settings.sandbox.read_only,
        );
        append_string_list(output, "sandbox.writable", &self.settings.sandbox.writable);
        _ = writeln!(
            output,
            "  skills: disabled={}, only={:?}, path={:?}",
            self.settings.skills.disabled, self.settings.skills.only, self.settings.skills.path
        );
        match &self.settings.tools_enabled {
            Some(names) => _ = writeln!(output, "  tools.enabled: {names:?}"),
            None => output.push_str("  tools.enabled: all\n"),
        }
        let limits = &self.settings.limits;
        _ = writeln!(
            output,
            "  limits: max_turns={}, max_tool_calls={}, bash_output_max_bytes={}, bash_read_cap={}, bash_session_output_max_bytes={}, bash_session_max={}, bash_session_max_seconds={}, bash_session_exited_ttl_seconds={}, read_default_end_line={}, read_max_output_bytes={}, edit_max_file_bytes={}, read_max_line_bytes={}, read_image_max_bytes={}, hook_output_limit={}",
            option_label(limits.max_turns, "uncapped"),
            option_label(limits.max_tool_calls, "uncapped"),
            option_label(limits.bash_output_max_bytes, "unlimited"),
            option_label(limits.bash_read_cap, "unlimited"),
            limits.bash_session_output_max_bytes,
            limits.bash_session_max,
            limits.bash_session_max_seconds,
            limits.bash_session_exited_ttl_seconds,
            option_label(limits.read_default_end_line, "unlimited"),
            option_label(limits.read_max_output_bytes, "unlimited"),
            option_label(limits.edit_max_file_bytes, "unlimited"),
            option_label(limits.read_max_line_bytes, "unlimited"),
            option_label(limits.read_image_max_bytes, "unlimited"),
            option_label(limits.hook_output_limit, "unlimited"),
        );
        let judge = &self.settings.judge;
        _ = writeln!(
            output,
            "  judge: enabled={}, model={:?}, timeout_secs={}, retry_budget_secs={}, rubric_file={:?}",
            judge.enabled,
            judge.model,
            judge.timeout_secs,
            judge.retry_budget_secs,
            judge.rubric_file
        );
    }

    fn append_trust(&self, output: &mut String) {
        _ = writeln!(
            output,
            "Project trust: {} (mode {})",
            self.trust.state.label(),
            self.trust.mode.label()
        );
        _ = writeln!(output, "  Authority ({}):", self.trust.authority.len());
        if self.trust.authority.is_empty() {
            output.push_str("    (none)\n");
        }
        for source in &self.trust.authority {
            let keys = if source.keys.is_empty() {
                String::new()
            } else {
                format!(" [{}]", source.keys.join(", "))
            };
            _ = writeln!(
                output,
                "    {} ({}){}",
                source.path,
                source.kind.label(),
                keys
            );
        }
        _ = writeln!(output, "  Changes ({}):", self.trust.changes.len());
        if self.trust.changes.is_empty() {
            output.push_str("    (none)\n");
        }
        for change in &self.trust.changes {
            _ = writeln!(output, "    {} ({})", change.path, change.change.label());
        }
    }

    fn append_prompt(&self, output: &mut String) {
        output.push_str("System prompt\n");
        let path = self.prompt.path.as_deref().unwrap_or("built-in");
        _ = writeln!(output, "  Source: {} ({path})", self.prompt.source);
        if let Some(configured) = &self.prompt.configured_path {
            _ = writeln!(output, "  Settings path: {configured}");
        }
    }

    fn append_grants(&self, output: &mut String) {
        output.push_str("Grants\n");
        _ = writeln!(
            output,
            "  Policy: {} (from {})",
            self.grants.policy.label(),
            self.grants.policy_source.label()
        );
        if self.grants.policy.applies_os_sandbox() {
            output.push_str("  OS sandbox: applied\n");
        } else {
            output.push_str("  OS sandbox: not applied (danger-full-access)\n");
        }
        _ = writeln!(
            output,
            "  Effective sets: {} read-write, {} read-only, {} sandbox-writable, {} sandbox-only-read, {} denied, {} user grants",
            self.grants.read_write.len(),
            self.grants.read_only.len(),
            self.grants.sandbox_writable.len(),
            self.grants.sandbox_only_read.len(),
            self.grants.denied.len(),
            self.grants.user_grants.len()
        );
        append_paths(output, "read_write", &self.grants.read_write);
        append_paths(output, "user_grants", &self.grants.user_grants);
        _ = writeln!(output, "  denied ({})", self.grants.denied.len());
        if self.grants.denied.is_empty() {
            output.push_str("    (none)\n");
        }
        for denied in &self.grants.denied {
            _ = writeln!(
                output,
                "    {} ({})",
                denied.path.display(),
                denied.kind.label()
            );
        }
        output.push_str("  Full sets and source lists: cake debug grants\n");
    }

    fn append_tools(&self, output: &mut String) {
        output.push_str("Tools\n");
        match &self.tools.selected {
            Some(names) => _ = writeln!(output, "  Selection: {} ({:?})", self.tools.source, names),
            None => _ = writeln!(output, "  Selection: {} (all tools)", self.tools.source),
        }
        _ = writeln!(
            output,
            "  Built-in ({}): {}",
            self.tools.builtin.len(),
            self.tools.builtin.join(", ")
        );
    }

    fn append_skills(&self, output: &mut String) {
        _ = writeln!(
            output,
            "Skills: {} ({}); selected {}, filtered {}",
            self.skills.selection,
            self.skills.reason,
            self.skills.selected.len(),
            self.skills.filtered.len()
        );
        if !self.skills.selected.is_empty() {
            output.push_str("  Selected\n");
            append_skill_rows(output, &self.skills.selected);
        }
        if !self.skills.filtered.is_empty() {
            output.push_str("  Filtered\n");
            append_skill_rows(output, &self.skills.filtered);
        }
    }

    fn append_hooks(&self, output: &mut String) {
        output.push_str("Hooks\n");
        for source in &self.hooks.sources {
            _ = writeln!(
                output,
                "  {} {}: {}{}",
                source.origin,
                source.path,
                source.status,
                if source.group_count == 0 {
                    String::new()
                } else {
                    format!(" ({} groups)", source.group_count)
                }
            );
        }
        if self.hooks.groups.is_empty() {
            output.push_str("  Groups: none\n");
        } else {
            _ = writeln!(output, "  Groups ({})", self.hooks.groups.len());
            for group in &self.hooks.groups {
                _ = writeln!(
                    output,
                    "    {} {} {} hooks, {} fail-closed ({})",
                    group.event,
                    group.matcher,
                    group.hook_count,
                    group.fail_closed_count,
                    group.source
                );
            }
        }
        output.push_str("  Commands: not shown by this report\n");
    }

    fn append_toolbox(&self, output: &mut String) {
        output.push_str("Toolbox (describe probing skipped)\n");
        if self.toolbox.directories.is_empty() {
            output.push_str("  Directories: none\n");
            return;
        }
        for directory in &self.toolbox.directories {
            _ = writeln!(
                output,
                "  {} ({}{}; {} candidates)",
                directory.path,
                if directory.project_local {
                    "project-local, "
                } else {
                    ""
                },
                directory.status,
                directory.candidates.len()
            );
            for candidate in &directory.candidates {
                _ = writeln!(output, "    {} ({})", candidate.filename, candidate.path);
            }
        }
    }

    fn append_diagnostics(&self, output: &mut String) {
        let checks = self.report_checks();
        if checks.is_empty() {
            output.push_str("Diagnostics: none\n");
            return;
        }
        output.push_str("Diagnostics\n");
        for check in checks {
            _ = writeln!(output, "{}: {}", check.status.label(), check.message);
        }
    }
}

fn append_skill_rows(output: &mut String, rows: &[SkillRow]) {
    for row in rows {
        _ = writeln!(output, "    {} ({}) {}", row.name, row.scope, row.location);
    }
}

fn append_string_list(output: &mut String, title: &str, values: &[String]) {
    _ = writeln!(output, "  {title} ({}): {values:?}", values.len());
}

fn append_paths(output: &mut String, title: &str, paths: &[PathBuf]) {
    _ = writeln!(output, "  {title} ({})", paths.len());
    if paths.is_empty() {
        output.push_str("    (none)\n");
        return;
    }
    for path in paths {
        _ = writeln!(output, "    {}", path.display());
    }
}

fn path_strings(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

fn option_label<T: std::fmt::Display>(value: Option<T>, none: &str) -> String {
    value.map_or_else(|| none.to_string(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_rows_sort_by_name_then_location() {
        let catalog = SkillCatalog {
            skills: vec![
                crate::config::skills::Skill {
                    name: "beta".to_string(),
                    description: "b".to_string(),
                    location: PathBuf::from("/b/SKILL.md"),
                    base_directory: PathBuf::from("/b"),
                    scope: crate::config::skills::SkillScope::User,
                },
                crate::config::skills::Skill {
                    name: "alpha".to_string(),
                    description: "a".to_string(),
                    location: PathBuf::from("/a/SKILL.md"),
                    base_directory: PathBuf::from("/a"),
                    scope: crate::config::skills::SkillScope::Project,
                },
            ],
            diagnostics: Vec::new(),
        };

        let rows = skill_rows(&catalog);

        assert_eq!(rows[0].name, "alpha");
        assert_eq!(rows[1].name, "beta");
        assert_eq!(rows[0].scope, "project");
    }

    #[test]
    fn hook_group_rows_never_include_command_text() {
        let group = HookGroup {
            event: crate::config::hooks::HookEvent::PreToolUse,
            matcher: HookMatcher::Exact(vec![crate::config::hooks::HookSource::Tool(
                "Bash".to_string(),
            )]),
            hooks: vec![crate::config::hooks::HookCommand {
                command: "touch /tmp/secret-marker".to_string(),
                timeout: std::time::Duration::from_secs(10),
                fail_closed: true,
                status_message: None,
                source_path: PathBuf::from("/hooks/hooks.json"),
            }],
        };
        let mut rows = Vec::new();

        append_groups(&mut rows, "/hooks/hooks.json", &[group]);

        let rendered = serde_json::to_string(&rows).expect("rows serialize");
        assert!(!rendered.contains("secret-marker"));
        assert_eq!(rows[0].matcher, "Bash");
        assert_eq!(rows[0].hook_count, 1);
        assert_eq!(rows[0].fail_closed_count, 1);
    }

    #[test]
    fn limits_snapshot_preserves_configured_values() {
        let limits = ResolvedLimits {
            max_turns: Some(9),
            max_tool_calls: None,
            tool_limits: crate::config::settings::ToolLimits {
                bash_output_max_bytes: Some(1234),
                ..crate::config::settings::ToolLimits::defaults()
            },
        };
        let snapshot = LimitsSnapshot::from(&limits);
        let value = serde_json::to_value(&snapshot).expect("limits serialize");
        assert_eq!(value["max_turns"], 9);
        assert_eq!(value["max_tool_calls"], serde_json::Value::Null);
        assert_eq!(value["bash_output_max_bytes"], 1234);
    }
}
