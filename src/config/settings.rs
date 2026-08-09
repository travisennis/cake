use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::expand_home;
use crate::config::model::{ApiType, ModelConfig, ModelProvider, ProviderHeaders, ReasoningEffort};
use crate::config::skills::SkillConfig;

/// Skill settings loaded from settings.toml.
///
/// Controls skill discovery and filtering behavior.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillSettings {
    /// If true, disable all skills
    #[serde(default)]
    pub disabled: bool,
    /// Optional list of skill names to load (empty = all)
    #[serde(default)]
    pub only: Vec<String>,
    /// Additional skill directories, separated by the platform path separator.
    #[serde(default)]
    pub path: Option<String>,
}

/// Partial skill settings used by profiles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillSettingsOverlay {
    /// If set, overrides whether skills are disabled.
    #[serde(default)]
    pub disabled: Option<bool>,
    /// If set, overrides the skill allowlist.
    #[serde(default)]
    pub only: Option<Vec<String>>,
    /// If set, overrides additional skill directories.
    #[serde(default)]
    pub path: Option<String>,
}

impl SkillSettings {
    fn apply_overlay(&mut self, overlay: SkillSettingsOverlay) {
        if let Some(disabled) = overlay.disabled {
            self.disabled = disabled;
        }
        if let Some(only) = overlay.only {
            self.only = only;
        }
        if overlay.path.is_some() {
            self.path = overlay.path;
        }
    }
}

/// Tool-scoped settings loaded from `settings.toml`.
///
/// Holds per-tool configuration tables. Currently only the Bash tool has
/// settings (`[tools.bash]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsSettings {
    /// Bash tool settings.
    #[serde(default)]
    pub bash: Option<BashToolSettings>,
}

/// Bash tool settings loaded from `[tools.bash]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BashToolSettings {
    /// LLM judge settings for Bash command safety (`[tools.bash.judge]`).
    #[serde(default)]
    pub judge: Option<BashJudgeSettings>,
}

/// TOML-facing LLM judge settings for the Bash tool.
///
/// Absent fields keep the value from lower-precedence settings files; the
/// resolved defaults live in [`JudgeSettings`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BashJudgeSettings {
    /// Judge model override. When unset, the judge uses the agent's configured
    /// model (same provider, same family).
    #[serde(default)]
    pub model: Option<String>,
    /// Bounded judge call timeout in seconds. When unset, defaults to 30.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Resolved LLM judge settings for Bash command safety.
///
/// Produced by [`SettingsLoader`] after merging global, project, and profile
/// settings with defaults applied. The allowlist and emergency bypass land in
/// `Milestone 4` of the LLM-judge `ExecPlan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeSettings {
    /// Judge model override. `None` means "use the agent's configured model".
    pub model: Option<String>,
    /// Bounded judge call timeout in seconds (default 30).
    pub timeout_secs: u64,
}

impl Default for JudgeSettings {
    fn default() -> Self {
        Self {
            model: None,
            timeout_secs: DEFAULT_JUDGE_TIMEOUT_SECS,
        }
    }
}

/// Default bounded judge timeout in seconds when no `[tools.bash.judge]`
/// setting is present.
pub const DEFAULT_JUDGE_TIMEOUT_SECS: u64 = 30;

/// Sandbox path grants loaded from settings.toml.
///
/// Controls which extra filesystem paths the Bash sandbox and the in-process
/// Read/Edit/Write/Grep path validation allow, on top of the built-in
/// toolchain paths, `--add-dir`, and `directories`.
///
/// Entries may be absolute or relative paths and support `~` expansion.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxSettings {
    /// Paths granted read + execute access. Entries may be files or
    /// directories.
    #[serde(default)]
    pub read_only: Vec<String>,
    /// Paths granted read + write + execute access. Entries may be files or
    /// directories.
    #[serde(default)]
    pub writable: Vec<String>,
}

/// Profile-specific behavior settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileSettings {
    /// Name of the model to use when `--model` is not specified.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Skill configuration overlay.
    #[serde(default)]
    pub skills: SkillSettingsOverlay,
    /// Additional directories for read-write access.
    #[serde(default)]
    pub directories: Vec<String>,
    /// Sandbox path grants overlaid onto the merged top-level `[sandbox]`
    /// section. Merged as a union, like `directories`.
    #[serde(default)]
    pub sandbox: Option<SandboxSettings>,
    /// Path to a custom system prompt file for this profile.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Model definitions are intentionally not supported in profiles.
    #[serde(default)]
    pub models: Option<Vec<ModelDefinition>>,
}

/// Root settings structure loaded from settings.toml.
///
/// Contains a list of model definitions, an optional default model name,
/// and optional additional directories for read-write access.
///
/// # Examples
///
/// Settings are loaded via [`SettingsLoader::load`] or
/// [`SettingsLoader::load_with_profile`] which merge global and project-level
/// settings files automatically.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Name of the model to use when `--model` is not specified
    #[serde(default)]
    pub default_model: Option<String>,
    /// List of model definitions
    #[serde(default)]
    pub models: Vec<ModelDefinition>,
    /// Skill configuration
    #[serde(default)]
    pub skills: Option<SkillSettings>,
    /// Additional directories for read-write access.
    /// Merged from global and project settings.
    #[serde(default)]
    pub directories: Vec<String>,
    /// Sandbox path grants. Merged as a union from global and project settings
    /// plus the selected profile, like `directories`.
    #[serde(default)]
    pub sandbox: Option<SandboxSettings>,
    /// Path to a custom system prompt file.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Named behavior profiles.
    #[serde(default)]
    pub profiles: HashMap<String, ProfileSettings>,
    /// Tool-scoped settings.
    #[serde(default)]
    pub tools: Option<ToolsSettings>,
}

/// Result of loading and merging settings from all sources.
///
/// Contains the merged model map, the resolved default model name,
/// and merged directories.
/// Separate from [`Settings`] to represent the post-merge state.
#[derive(Debug, Clone)]
pub struct LoadedSettings {
    /// Map of model name to definition (global + project merged)
    pub models: HashMap<String, ModelDefinition>,
    /// Name of the default model from the highest-precedence settings file
    pub default_model: Option<String>,
    /// Additional directories for read-write access (global + project merged)
    pub directories: Vec<String>,
    /// Effective sandbox path grants (global + project + selected profile),
    /// merged as a union.
    pub sandbox: SandboxSettings,
    /// Effective skill settings (global + project + selected profile)
    pub skills: SkillSettings,
    /// Resolved system prompt path from settings (global + project + selected profile).
    /// This is a path to a custom prompt file, checked after `.cake/system.md`
    /// but before `~/.config/cake/system.md` in the precedence chain.
    pub system_prompt: Option<String>,
    /// Effective LLM judge settings (global + project merged, defaults applied).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "judge settings are consumed by ExecPlan Milestones 3 and 5"
        )
    )]
    pub judge: JudgeSettings,
}

/// Definition of a named model in settings.toml.
///
/// Each model has a unique name that can be used with `--model <name>`
/// to select a specific model configuration.
///
/// # Examples
///
/// Model definitions are declared in `settings.toml` under `[[models]]`.
/// Use `SettingsLoader::load` to obtain a `LoadedSettings` map and access
/// definitions by name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDefinition {
    /// Unique name for the model (lowercase alphanumeric + hyphens only)
    pub name: String,
    /// Model identifier (e.g. "glm-5.1", "anthropic/claude-3-sonnet")
    pub model: String,
    /// Base URL for the API endpoint (required)
    pub base_url: String,
    /// Name of the environment variable containing the API key (required)
    pub api_key_env: String,
    /// Provider strategy to use. If unset, cake may infer from `base_url`.
    #[serde(default)]
    pub provider: Option<ModelProvider>,
    /// Structured provider-specific request headers.
    #[serde(default)]
    pub provider_headers: Option<ProviderHeaders>,
    /// Which API format to use
    #[serde(default)]
    pub api_type: ApiType,
    /// Sampling temperature
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Nucleus sampling parameter
    #[serde(default)]
    pub top_p: Option<f32>,
    /// Maximum number of output tokens
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// Reasoning effort level
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Reasoning summary mode (Responses API only)
    #[serde(default)]
    pub reasoning_summary: Option<String>,
    /// Maximum reasoning tokens budget
    #[serde(default)]
    pub reasoning_max_tokens: Option<u32>,
    /// Provider routing hints
    #[serde(default)]
    pub providers: Vec<String>,
}

impl ModelDefinition {
    /// Validates the model name format.
    ///
    /// Model names must be lowercase alphanumeric with hyphens only.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// assert!(ModelDefinition::validate_name("my-model").is_ok());
    /// assert!(ModelDefinition::validate_name("Invalid").is_err());
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `SettingsError::InvalidModelName` if the name is empty or
    /// contains invalid characters.
    pub fn validate_name(name: &str) -> Result<(), SettingsError> {
        if name.is_empty() {
            return Err(SettingsError::InvalidModelName {
                name: name.to_string(),
                reason: "name cannot be empty".to_string(),
            });
        }

        let valid_chars = name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');

        if !valid_chars {
            return Err(SettingsError::InvalidModelName {
                name: name.to_string(),
                reason: "must contain only lowercase letters, numbers, and hyphens".to_string(),
            });
        }

        Ok(())
    }

    /// Converts the model definition to a `ModelConfig`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let config = def.to_model_config();
    /// assert_eq!(config.model, "test/model");
    /// ```
    pub fn to_model_config(&self) -> ModelConfig {
        ModelConfig {
            model: self.model.clone(),
            api_type: self.api_type,
            base_url: self.base_url.clone(),
            api_key_env: self.api_key_env.clone(),
            provider: self.provider,
            provider_headers: self.provider_headers.clone(),
            temperature: self.temperature,
            top_p: self.top_p,
            max_output_tokens: self.max_output_tokens,
            reasoning_effort: self.reasoning_effort,
            reasoning_summary: self.reasoning_summary.clone(),
            reasoning_max_tokens: self.reasoning_max_tokens,
            providers: self.providers.clone(),
        }
    }
}

/// Errors that can occur when loading or processing settings
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("Invalid model name '{name}': {reason}")]
    InvalidModelName { name: String, reason: String },

    #[error("Duplicate model name '{name}' in settings")]
    DuplicateModelName { name: String },

    #[error("Invalid profile name '{name}': {reason}")]
    InvalidProfileName { name: String, reason: String },

    #[error("Profile '{name}' defines models, but model configs are only supported at top level")]
    ProfileModelsUnsupported { name: String },

    #[error("Unknown profile '{name}'{available}. Define [profiles.{name}] in settings.toml.")]
    UnknownProfile { name: String, available: String },

    #[error(
        "Default model '{name}' not found in models list. \
         Define a [[models]] entry with name = \"{name}\", \
         or change default_model to an existing model name."
    )]
    DefaultModelNotFound { name: String },

    #[error("Failed to parse settings file: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("Failed to read settings file: {0}")]
    IoError(#[from] std::io::Error),
}

/// Loader for settings from TOML files.
///
/// Settings are loaded from both global and project-level files,
/// with project settings taking precedence over global settings.
///
/// # Examples
///
/// ```ignore
/// let loaded = SettingsLoader::load(Some(Path::new("/project")))?;
/// if let Some(model) = loaded.models.get("zen") {
///     println!("Model: {}", model.model);
/// }
/// ```
pub struct SettingsLoader;

impl SettingsLoader {
    /// Load settings from a TOML file at the given path.
    /// Returns Ok(None) if the file doesn't exist.
    /// Returns an error if the file exists but is invalid.
    fn load_file(path: &Path) -> Result<Option<Settings>, SettingsError> {
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(path)?;
        let settings: Settings = toml::from_str(&content)?;
        Ok(Some(settings))
    }

    /// Resolve skill configuration from CLI flags and settings.
    ///
    /// Precedence (highest to lowest):
    /// 1. `--no-skills` CLI flag
    /// 2. `--skills name1,name2` CLI flag
    /// 3. `skills.only` in settings
    /// 4. `skills.disabled = true` in settings
    /// 5. Default: load all discovered skills
    pub fn resolve_skill_config(
        no_skills: bool,
        skills_flag: Option<&str>,
        settings: &SkillSettings,
    ) -> SkillConfig {
        if no_skills {
            return SkillConfig::Disabled;
        }

        if let Some(names) = skills_flag {
            let skill_names: Vec<String> = names
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if skill_names.is_empty() {
                return SkillConfig::Disabled;
            }
            return SkillConfig::Only(skill_names);
        }

        if !settings.only.is_empty() {
            return SkillConfig::Only(settings.only.clone());
        }

        if settings.disabled {
            return SkillConfig::Disabled;
        }

        SkillConfig::All
    }

    /// Loads and merges settings from global and project locations.
    ///
    /// Settings are loaded from:
    /// 1. Global settings: `~/.config/cake/settings.toml`
    /// 2. Project settings: `{project_dir}/.cake/settings.toml`
    ///
    /// Project settings override global settings for models with the same name
    /// and for `default_model`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let loaded = SettingsLoader::load(Some(Path::new("/my/project")))?;
    /// if let Some(model) = loaded.models.get("default") {
    ///     println!("Model: {}", model.model);
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if a settings file exists but cannot be parsed,
    /// if duplicate model names are found within the same file,
    /// or if `default_model` references a model that doesn't exist.
    pub fn load(project_dir: Option<&Path>) -> Result<LoadedSettings, SettingsError> {
        Self::load_with_profile(project_dir, None)
    }

    /// Loads and merges settings, applying the selected profile if provided.
    pub fn load_with_profile(
        project_dir: Option<&Path>,
        profile: Option<&str>,
    ) -> Result<LoadedSettings, SettingsError> {
        let mut acc = SettingsAccumulator::default();

        // Load global settings first, then project settings (they override).
        let global_path = crate::config::config_dir()
            .join("cake")
            .join("settings.toml");
        if let Some(settings) = Self::load_file(&global_path)? {
            Self::validate_profiles(&settings.profiles)?;
            Self::merge_settings(settings, &mut acc)?;
        }
        if let Some(project_dir) = project_dir {
            let project_path = project_dir.join(".cake").join("settings.toml");
            if let Some(settings) = Self::load_file(&project_path)? {
                Self::validate_profiles(&settings.profiles)?;
                Self::merge_settings(settings, &mut acc)?;
            }
        }

        if let Some(name) = profile {
            if let Err(e) = ModelDefinition::validate_name(name) {
                return Err(SettingsError::InvalidProfileName {
                    name: name.to_string(),
                    reason: e.to_string(),
                });
            }

            let Some(overlays) = acc.profiles.get(name) else {
                let mut available: Vec<_> = acc.profiles.keys().cloned().collect();
                available.sort();
                let available = if available.is_empty() {
                    String::new()
                } else {
                    format!(". Available profiles: {}", available.join(", "))
                };
                return Err(SettingsError::UnknownProfile {
                    name: name.to_string(),
                    available,
                });
            };

            for overlay in overlays.clone() {
                acc.apply_profile_overlay(&overlay);
            }
        }

        // Validate that default_model (if set) refers to an existing model.
        if let Some(ref name) = acc.default_model
            && !acc.models.contains_key(name.as_str())
        {
            return Err(SettingsError::DefaultModelNotFound { name: name.clone() });
        }

        Ok(acc.into_loaded())
    }

    fn merge_settings(
        settings: Settings,
        acc: &mut SettingsAccumulator,
    ) -> Result<(), SettingsError> {
        Self::add_models_to_map(&mut acc.models, settings.models)?;
        if settings.default_model.is_some() {
            acc.default_model = settings.default_model;
        }
        for dir in settings.directories {
            acc.directories.insert(expand_home_str(&dir));
        }
        // `unwrap_or_default` keeps this function at baseline complexity: an
        // absent `[sandbox]` section contributes no grants.
        let sandbox = settings.sandbox.unwrap_or_default();
        acc.sandbox_read_only
            .extend(sandbox.read_only.iter().map(|p| expand_home_str(p)));
        acc.sandbox_writable
            .extend(sandbox.writable.iter().map(|p| expand_home_str(p)));
        if let Some(settings_skills) = settings.skills {
            acc.skills = settings_skills;
        }
        if settings.system_prompt.is_some() {
            acc.system_prompt = settings.system_prompt;
        }
        Self::merge_judge_settings(settings.tools, acc);
        for (name, profile) in settings.profiles {
            acc.profiles.entry(name).or_default().push(profile);
        }
        Ok(())
    }

    /// Merge `[tools.bash.judge]` fields into the accumulator.
    ///
    /// Extracted so [`Self::merge_settings`] stays at baseline complexity.
    /// Absent fields keep lower-precedence values; defaults are applied later
    /// in [`SettingsAccumulator::into_loaded`].
    fn merge_judge_settings(tools: Option<ToolsSettings>, acc: &mut SettingsAccumulator) {
        let Some(judge) = tools.and_then(|t| t.bash).and_then(|b| b.judge) else {
            return;
        };
        if judge.model.is_some() {
            acc.judge_model = judge.model;
        }
        if judge.timeout_secs.is_some() {
            acc.judge_timeout_secs = judge.timeout_secs;
        }
    }

    fn validate_profiles(profiles: &HashMap<String, ProfileSettings>) -> Result<(), SettingsError> {
        for (name, profile) in profiles {
            if let Err(e) = ModelDefinition::validate_name(name) {
                return Err(SettingsError::InvalidProfileName {
                    name: name.clone(),
                    reason: e.to_string(),
                });
            }
            if profile.models.is_some() {
                return Err(SettingsError::ProfileModelsUnsupported { name: name.clone() });
            }
        }
        Ok(())
    }

    /// Add models from a settings file to the map.
    ///
    /// Checks for duplicate names within the same file (errors if found).
    /// Allows overriding models from previous files (e.g., global settings).
    fn add_models_to_map(
        map: &mut HashMap<String, ModelDefinition>,
        definitions: Vec<ModelDefinition>,
    ) -> Result<(), SettingsError> {
        // First, check for duplicates within the same file
        let mut seen: HashSet<&str> = HashSet::new();
        for def in &definitions {
            // Validate name format
            if let Err(e) = ModelDefinition::validate_name(&def.name) {
                return Err(SettingsError::InvalidModelName {
                    name: def.name.clone(),
                    reason: e.to_string(),
                });
            }

            // Check for duplicates within this file
            if !seen.insert(def.name.as_str()) {
                return Err(SettingsError::DuplicateModelName {
                    name: def.name.clone(),
                });
            }
        }

        // Now add all models to the map (overwriting any existing entries)
        for def in definitions {
            let name = def.name.clone();
            map.insert(name, def);
        }

        Ok(())
    }
}

/// Mutable merge state shared by [`SettingsLoader`] while combining global,
/// project, and profile settings.
#[derive(Default)]
struct SettingsAccumulator {
    models: HashMap<String, ModelDefinition>,
    default_model: Option<String>,
    directories: HashSet<String>,
    sandbox_read_only: HashSet<String>,
    sandbox_writable: HashSet<String>,
    skills: SkillSettings,
    profiles: HashMap<String, Vec<ProfileSettings>>,
    system_prompt: Option<String>,
    judge_model: Option<String>,
    judge_timeout_secs: Option<u64>,
}

impl SettingsAccumulator {
    /// Union a profile overlay into the accumulated settings.
    fn apply_profile_overlay(&mut self, overlay: &ProfileSettings) {
        if let Some(ref profile_default) = overlay.default_model {
            self.default_model = Some(profile_default.clone());
        }
        self.system_prompt.clone_from(&overlay.system_prompt);
        self.directories
            .extend(overlay.directories.iter().map(|d| expand_home_str(d)));
        if let Some(ref overlay_sandbox) = overlay.sandbox {
            self.sandbox_read_only
                .extend(overlay_sandbox.read_only.iter().map(|p| expand_home_str(p)));
            self.sandbox_writable
                .extend(overlay_sandbox.writable.iter().map(|p| expand_home_str(p)));
        }
        self.skills.apply_overlay(overlay.skills.clone());
    }

    /// Convert the accumulated merge state into the final [`LoadedSettings`].
    fn into_loaded(self) -> LoadedSettings {
        LoadedSettings {
            models: self.models,
            default_model: self.default_model,
            directories: self.directories.into_iter().collect(),
            sandbox: SandboxSettings {
                read_only: self.sandbox_read_only.into_iter().collect(),
                writable: self.sandbox_writable.into_iter().collect(),
            },
            skills: self.skills,
            system_prompt: self.system_prompt,
            judge: JudgeSettings {
                model: self.judge_model,
                timeout_secs: self
                    .judge_timeout_secs
                    .unwrap_or(DEFAULT_JUDGE_TIMEOUT_SECS),
            },
        }
    }
}

/// Expand `~` in a settings path string, returning the expanded string.
fn expand_home_str(path: &str) -> String {
    expand_home(PathBuf::from(path))
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
