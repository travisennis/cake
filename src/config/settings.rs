use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::config::blob_store::TOOL_OUTPUT_PRODUCER_LIMIT_BYTES;
use crate::config::expand_home;
use crate::config::model::{
    ApiType, ModelConfig, ModelProvider, ProviderHeaders, ReasoningEffort, ResolvedModelConfig,
};
use crate::config::skills::SkillConfig;
use crate::config::trust::{
    AuthorityKind, AuthoritySource, ConfigLoadError, ProjectConfigMode, ProjectConfigSnapshot,
    ProjectTrustReport,
};

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
    /// Maximum rendered skill catalog characters. Absent uses the
    /// context-relative default; the value must be positive.
    #[serde(default)]
    pub catalog_budget: Option<PositiveLimit>,
    /// Maximum characters in each skill routing description. Absent uses the
    /// compiled default of 400.
    #[serde(default)]
    pub description_budget: Option<PositiveLimit>,
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
    /// If set, overrides the rendered catalog budget.
    #[serde(default)]
    pub catalog_budget: Option<PositiveLimit>,
    /// If set, overrides the per-description budget.
    #[serde(default)]
    pub description_budget: Option<PositiveLimit>,
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
        if overlay.catalog_budget.is_some() {
            self.catalog_budget = overlay.catalog_budget;
        }
        if overlay.description_budget.is_some() {
            self.description_budget = overlay.description_budget;
        }
    }

    /// Resolved catalog character budget, when configured.
    #[must_use]
    pub fn catalog_budget(&self) -> Option<usize> {
        self.catalog_budget
            .map(|limit| usize::try_from(limit.value()).unwrap_or(usize::MAX))
    }

    /// Resolved per-description character budget, when configured.
    #[must_use]
    pub fn description_budget(&self) -> Option<usize> {
        self.description_budget
            .map(|limit| usize::try_from(limit.value()).unwrap_or(usize::MAX))
    }
}

/// Tool-scoped settings loaded from `settings.toml`.
///
/// `enabled` selects the exact model-visible tools. The optional list preserves
/// the distinction between an absent setting (use all tools) and an explicit
/// empty list (use no tools). Per-tool configuration currently only includes
/// the Bash tool's judge settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsSettings {
    /// Exact registered tool names to expose; absent means all tools.
    #[serde(default)]
    pub enabled: Option<Vec<String>>,
    /// Bash tool settings.
    #[serde(default)]
    pub bash: Option<BashToolSettings>,
}

/// Tool settings overlay used by profiles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsSettingsOverlay {
    /// If set, replaces the inherited exact tool-name selection.
    #[serde(default)]
    pub enabled: Option<Vec<String>>,
    /// Bash tool settings overlay.
    #[serde(default)]
    pub bash: Option<ProfileBashSettings>,
}

/// Profile-only observational settings; primary judge policy remains top-level.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileBashSettings {
    pub gates: Option<Vec<String>>,
    pub judge: Option<ProfileJudgeSettings>,
}

/// A profile can select the `TypeSafe` evaluation mode, model, and deadline;
/// the fast-approval cutoff is compiled, not overridable here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileJudgeSettings {
    pub typesafe: Option<TypeSafeSettingsOverlay>,
}

/// Bash tool settings loaded from `[tools.bash]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BashToolSettings {
    pub gates: Option<Vec<String>>,
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
    /// Judge model override: the `name` of a `[[models]]` entry, resolved to
    /// that model's full configuration (provider, base URL, API key,
    /// temperature, reasoning, and other fields) — the same vocabulary as
    /// `default_model` and `--model`. When unset, the judge uses the agent's
    /// configured model (same provider, same family).
    #[serde(default)]
    pub model: Option<String>,
    /// Bounded judge call timeout in seconds. When unset, defaults to 30.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Extra seconds one recovery attempt may consume beyond `timeout_secs`.
    /// When unset, defaults to 15; 0 disables recovery entirely.
    #[serde(default)]
    pub retry_budget_secs: Option<u64>,
    /// Optional path to a user rubric file whose text is appended to the
    /// embedded default rubric as additional judge guidance.
    #[serde(default)]
    pub rubric_file: Option<PathBuf>,
    /// Emergency bypass: `false` disables the judge for every command,
    /// equivalent to `CAKE_JUDGE=off`. Off by default (the judge is enabled).
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Exact raw-command strings whose `block` verdicts are overridden to
    /// allow. An allowlisted command is still judged; the verdict and the
    /// `overridden` flag are recorded. No entries in shipped defaults.
    #[serde(default)]
    pub allowlist: Option<Vec<String>>,
    /// Optional `TypeSafe` evaluator (shadow or cascade).
    #[serde(default)]
    pub typesafe: Option<TypeSafeSettingsOverlay>,
}

/// Partial `TypeSafe` evaluator settings used by settings overlays.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TypeSafeSettingsOverlay {
    #[serde(default)]
    pub mode: Option<TypeSafeMode>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// `TypeSafe` evaluation mode.
///
/// `Shadow` observes beside the judge and never changes a decision. `Cascade`
/// makes the observation the first approval stage: a clean observation at or
/// above the reviewed cutoff approves the command without a judge call, and
/// every other outcome falls back to the judge. The cutoff itself is
/// compiled, so no mode value can lower it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TypeSafeMode {
    #[default]
    Off,
    Shadow,
    Cascade,
}

/// Resolved `TypeSafe` evaluator configuration: an observation under `shadow`,
/// an approval stage under `cascade`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeSafeSettings {
    pub mode: TypeSafeMode,
    pub model: String,
    pub timeout_ms: u64,
}

impl Default for TypeSafeSettings {
    fn default() -> Self {
        Self {
            mode: TypeSafeMode::Off,
            model: "jev-1.13.0".to_string(),
            timeout_ms: 1_000,
        }
    }
}

impl TypeSafeSettingsOverlay {
    /// Apply every present overlay field to the resolved settings.
    fn apply_to(self, settings: &mut TypeSafeSettings) {
        if let Some(mode) = self.mode {
            settings.mode = mode;
        }
        if let Some(model) = self.model {
            settings.model = model;
        }
        if let Some(timeout_ms) = self.timeout_ms {
            settings.timeout_ms = timeout_ms.max(1);
        }
    }
}

/// Resolved LLM judge settings for Bash command safety.
///
/// Produced by [`SettingsLoader`] after merging global, project, and profile
/// settings with defaults applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeSettings {
    /// Judge model override: the `name` of a `[[models]]` entry (the
    /// `default_model`/`--model` vocabulary), resolved to that model's full
    /// configuration. `None` means "use the agent's configured model".
    pub model: Option<String>,
    /// Bounded judge call timeout in seconds (default 30; never below 1). One
    /// judge provider call may consume at most this long.
    pub timeout_secs: u64,
    /// Extra seconds one recovery attempt may consume beyond `timeout_secs`
    /// (default 15). 0 disables recovery. The complete judge operation is
    /// bounded by `timeout_secs + retry_budget_secs`.
    pub retry_budget_secs: u64,
    /// Optional path to a user rubric file appended to the embedded default
    /// rubric. Relative paths resolve from the invocation working directory.
    pub rubric_file: Option<PathBuf>,
    /// Emergency bypass: `false` disables the judge for every command
    /// (equivalent to `CAKE_JUDGE=off`). Default true; off by default means
    /// the judge is enabled.
    pub enabled: bool,
    /// Exact raw-command strings whose `block` verdict is overridden to allow.
    /// An allowlisted command is still judged; the verdict and the `overridden`
    /// flag are recorded for telemetry. Empty by default.
    pub allowlist: Vec<String>,
    /// Optional bounded `TypeSafe` evaluation (shadow or cascade).
    pub typesafe: TypeSafeSettings,
}

impl Default for JudgeSettings {
    fn default() -> Self {
        Self {
            model: None,
            timeout_secs: DEFAULT_JUDGE_TIMEOUT_SECS,
            retry_budget_secs: DEFAULT_JUDGE_RETRY_BUDGET_SECS,
            rubric_file: None,
            enabled: true,
            allowlist: Vec::new(),
            typesafe: TypeSafeSettings::default(),
        }
    }
}

/// Default bounded judge timeout in seconds when no `[tools.bash.judge]`
/// setting is present.
pub const DEFAULT_JUDGE_TIMEOUT_SECS: u64 = 30;

/// Default extra seconds one recovery attempt may consume beyond
/// `timeout_secs`. 0 disables recovery entirely.
pub const DEFAULT_JUDGE_RETRY_BUDGET_SECS: u64 = 15;

/// Floor for the judge timeout in seconds. A `0` timeout would expire before
/// the judge request is polled and fail every command closed; values below
/// the floor are raised to it, matching the Bash tool's own timeout clamp.
const JUDGE_TIMEOUT_MIN_SECS: u64 = 1;

/// Sandbox path grants loaded from settings.toml.
///
/// Controls which extra filesystem paths the Bash sandbox and the in-process
/// Read/ReadImage/Edit/Write path validation allow, on top of the built-in
/// toolchain and system paths, `--add-dir`, `directories`, skill directories,
/// the user skill root, the global config directory, and Cake's own state
/// roots.
///
/// `Read` and `ReadImage` accept read-only and read-write grants. `Edit` and
/// `Write` accept only read-write grants. Credential-bearing paths are denied
/// before any ordinary grant.
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
    #[serde(default)]
    pub gates: std::collections::BTreeMap<String, crate::config::gates::GateSettings>,
    /// Name of the model to use when `--model` is not specified.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Skill configuration overlay.
    #[serde(default)]
    pub skills: SkillSettingsOverlay,
    /// Tool selection overlay.
    #[serde(default)]
    pub tools: ToolsSettingsOverlay,
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
    /// Limits overlayed onto the merged top-level `[limits]` section. Each
    /// configured key overrides the lower-precedence value.
    #[serde(default)]
    pub limits: Option<LimitsSettingsOverlay>,
    /// Per-request image budgets overlaid onto the merged top-level
    /// `[images]` section. Each configured key overrides the
    /// lower-precedence value.
    #[serde(default)]
    pub images: Option<ImagesSettingsOverlay>,
    /// Automatic compaction policy overlaid onto the merged top-level
    /// `[compaction]` section. Each configured key overrides the
    /// lower-precedence value.
    #[serde(default)]
    pub compaction: Option<CompactionSettingsOverlay>,
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
/// Settings are loaded via [`SettingsLoader::load_with_project_config`],
/// which merges global and trusted project settings under a
/// [`ProjectConfigMode`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub gates: std::collections::BTreeMap<String, crate::config::gates::GateSettings>,
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
    /// Agent-loop and tool output limits.
    #[serde(default)]
    pub limits: Option<LimitsSettingsOverlay>,
    /// Per-request image budgets.
    #[serde(default)]
    pub images: Option<ImagesSettingsOverlay>,
    /// Automatic pre-request compaction policy.
    #[serde(default)]
    pub compaction: Option<CompactionSettingsOverlay>,
}

impl Settings {
    /// Return a copy with every authority-bearing project key removed.
    ///
    /// Low-authority fields (`default_model`, `[limits]`, `[images]`,
    /// `[compaction]`, narrowing `[tools].enabled`, profile selection, and
    /// skill filters) are preserved.
    #[must_use]
    pub fn without_authority(&self) -> Self {
        let mut stripped = self.clone();
        stripped.gates.clear();
        stripped.directories.clear();
        stripped.sandbox = None;
        stripped.system_prompt = None;
        if let Some(skills) = &mut stripped.skills {
            skills.path = None;
        }
        if let Some(tools) = &mut stripped.tools {
            tools.bash = None;
        }
        for profile in stripped.profiles.values_mut() {
            profile.gates.clear();
            profile.directories.clear();
            profile.sandbox = None;
            profile.system_prompt = None;
            profile.skills.path = None;
            profile.tools.bash = None;
        }
        stripped
    }
}

/// Helpers for classifying authority-bearing settings keys.
pub struct ProjectAuthority;

impl ProjectAuthority {
    /// Report every authority-bearing key in one project settings source.
    ///
    /// The same function is used for review, status, and enforcement so a key
    /// cannot be trusted in one place and applied in another.
    #[must_use]
    pub fn from_settings(settings: &Settings, path: &Path) -> Vec<AuthoritySource> {
        let mut keys = Self::top_level_keys(settings);
        let mut profile_names: Vec<&String> = settings.profiles.keys().collect();
        profile_names.sort();
        for name in profile_names {
            if let Some(profile) = settings.profiles.get(name) {
                keys.extend(Self::profile_keys(name, profile));
            }
        }
        if keys.is_empty() {
            Vec::new()
        } else {
            vec![AuthoritySource {
                path: path.display().to_string(),
                kind: AuthorityKind::Settings,
                keys,
                executable: false,
            }]
        }
    }

    /// Authority-bearing top-level settings keys.
    fn top_level_keys(settings: &Settings) -> Vec<String> {
        let mut keys = Vec::new();
        if !settings.directories.is_empty() {
            keys.push("directories".to_string());
        }
        Self::sandbox_keys(settings.sandbox.as_ref(), None, &mut keys);
        keys.extend(settings.gates.keys().map(|name| format!("[gates.{name}]")));
        let (gates, judge) = settings
            .tools
            .as_ref()
            .and_then(|tools| tools.bash.as_ref())
            .map_or((false, false), |bash| {
                (bash.gates.is_some(), bash.judge.is_some())
            });
        Self::bash_keys(gates, judge, None, &mut keys);
        if settings.system_prompt.is_some() {
            keys.push("system_prompt".to_string());
        }
        if settings
            .skills
            .as_ref()
            .is_some_and(|skills| skills.path.is_some())
        {
            keys.push("[skills].path".to_string());
        }
        keys
    }

    /// Append sandbox grant keys, optionally namespaced to a profile.
    fn sandbox_keys(
        sandbox: Option<&SandboxSettings>,
        profile: Option<&str>,
        keys: &mut Vec<String>,
    ) {
        let Some(sandbox) = sandbox else {
            return;
        };
        let prefix = profile.map_or_else(String::new, |name| format!("profiles.{name}."));
        if !sandbox.read_only.is_empty() {
            keys.push(format!("[{prefix}sandbox].read_only"));
        }
        if !sandbox.writable.is_empty() {
            keys.push(format!("[{prefix}sandbox].writable"));
        }
    }

    /// Append Bash gate and legacy judge keys, optionally namespaced to a profile.
    fn bash_keys(gates: bool, judge: bool, profile: Option<&str>, keys: &mut Vec<String>) {
        let prefix = profile.map_or_else(String::new, |name| format!("profiles.{name}."));
        if gates {
            keys.push(format!("[{prefix}tools.bash].gates"));
        }
        if judge {
            keys.push(format!("[{prefix}tools.bash.judge]"));
        }
    }

    /// Authority-bearing keys inside one profile overlay.
    fn profile_keys(name: &str, profile: &ProfileSettings) -> Vec<String> {
        let mut keys = Vec::new();
        if !profile.directories.is_empty() {
            keys.push(format!("[profiles.{name}].directories"));
        }
        Self::sandbox_keys(profile.sandbox.as_ref(), Some(name), &mut keys);
        keys.extend(
            profile
                .gates
                .keys()
                .map(|gate| format!("[profiles.{name}.gates.{gate}]")),
        );
        let (gates, judge) = profile.tools.bash.as_ref().map_or((false, false), |bash| {
            (bash.gates.is_some(), bash.judge.is_some())
        });
        Self::bash_keys(gates, judge, Some(name), &mut keys);
        if profile.system_prompt.is_some() {
            keys.push(format!("[profiles.{name}].system_prompt"));
        }
        if profile.skills.path.is_some() {
            keys.push(format!("[profiles.{name}.skills].path"));
        }
        keys
    }
}

/// A single user-configured agent-loop resource limit.
///
/// A limit is either a positive-integer cap or the explicit `"unlimited"`
/// sentinel (no cap). The sentinel is a real value, not an absent key, so a
/// project can override a global cap back to uncapped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limit(Option<u32>);

impl Serialize for Limit {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            Some(value) => serializer.serialize_u32(value),
            None => serializer.serialize_str("unlimited"),
        }
    }
}

impl Limit {
    /// The explicit `"unlimited"` sentinel: no cap.
    pub const UNLIMITED: Self = Self(None);

    /// A positive-integer cap.
    pub const fn max(value: u32) -> Self {
        Self(Some(value))
    }

    /// The configured cap, or `None` when unlimited.
    pub const fn value(self) -> Option<u32> {
        self.0
    }
}

impl<'de> Deserialize<'de> for Limit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LimitVisitor;

        impl Visitor<'_> for LimitVisitor {
            type Value = Limit;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a positive integer or the string \"unlimited\"")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value == 0 {
                    return Err(E::custom(
                        "limit must be a positive integer or \"unlimited\"; 0 is not a valid limit",
                    ));
                }
                u32::try_from(value)
                    .ok()
                    .map(Limit::max)
                    .ok_or_else(|| E::custom("limit exceeds the maximum supported value (u32)"))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value <= 0 {
                    return Err(E::custom(
                        "limit must be a positive integer or \"unlimited\"; 0 and negative values are not valid limits",
                    ));
                }
                u64::try_from(value)
                    .ok()
                    .and_then(|v| u32::try_from(v).ok())
                    .map(Limit::max)
                    .ok_or_else(|| E::custom("limit exceeds the maximum supported value (u32)"))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.eq_ignore_ascii_case("unlimited") {
                    Ok(Limit::UNLIMITED)
                } else {
                    Err(E::custom(
                        "limit must be a positive integer or the string \"unlimited\"",
                    ))
                }
            }
        }

        deserializer.deserialize_any(LimitVisitor)
    }
}

/// A mandatory positive bound. Session safety limits and skill budgets cannot
/// be unlimited.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(try_from = "u32")]
pub struct PositiveLimit(u32);

impl TryFrom<u32> for PositiveLimit {
    type Error = &'static str;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if value == 0 {
            Err("limit must be a positive integer")
        } else {
            Ok(Self(value))
        }
    }
}

impl PositiveLimit {
    /// Return the configured positive value.
    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// TOML-facing resource-limit overlay.
///
/// The outer `Option` on each field means the setting key was absent. Output
/// budgets use [`Limit`] to allow `"unlimited"`; Bash session bounds use
/// [`PositiveLimit`] because they must stay finite. [`ResolvedLimits`] is used
/// at runtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LimitsSettingsOverlay {
    /// Maximum number of agent-loop turns before the run terminates with a
    /// limit-exceeded outcome. `None` means the key is absent (inherit); an
    /// explicit `"unlimited"` removes the cap.
    #[serde(default)]
    pub max_turns: Option<Limit>,
    /// Maximum number of tool calls executed before the run terminates with
    /// a limit-exceeded outcome. `None` means the key is absent (inherit); an
    /// explicit `"unlimited"` removes the cap.
    #[serde(default)]
    pub max_tool_calls: Option<Limit>,
    /// Maximum bytes of Bash tool output returned inline before the rest
    /// spills to a temp file. Absent uses the shared producer cap;
    /// `"unlimited"` is clamped to that cap.
    #[serde(default)]
    pub bash_output_max_bytes: Option<Limit>,
    /// Maximum bytes of Bash output read before the process is killed and the
    /// capture ends. Retained for settings compatibility; session-managed Bash
    /// commands do not use it.
    #[serde(default)]
    pub bash_read_cap: Option<Limit>,
    /// Maximum unread bytes retained per running Bash session. Absent uses the
    /// shared producer cap; configured values above it are clamped.
    #[serde(default)]
    pub bash_session_output_max_bytes: Option<PositiveLimit>,
    /// Maximum concurrent Bash sessions.
    #[serde(default)]
    pub bash_session_max: Option<PositiveLimit>,
    /// Hard wall clock for a Bash session, in seconds.
    #[serde(default)]
    pub bash_session_max_seconds: Option<PositiveLimit>,
    /// Retention period for completed Bash sessions, in seconds.
    #[serde(default)]
    pub bash_session_exited_ttl_seconds: Option<PositiveLimit>,
    /// Default Read window in lines when the model omits `end_line`. Absent
    /// uses the compiled default of 500; `"unlimited"` reads to the end of
    /// the file.
    #[serde(default)]
    pub read_default_end_line: Option<Limit>,
    /// Maximum bytes of Read output before it is truncated at a line
    /// boundary. Absent uses the compiled default of 100,000; `"unlimited"`
    /// disables the cap.
    #[serde(default)]
    pub read_max_output_bytes: Option<Limit>,
    /// Maximum bytes of an input file Edit will read. Absent uses the
    /// compiled default of 16 MiB; `"unlimited"` disables the cap.
    #[serde(default)]
    pub edit_max_file_bytes: Option<Limit>,
    /// Maximum bytes of one Read line delivered before it is truncated.
    /// Absent uses the compiled default of 10,000; `"unlimited"` re-enables
    /// reading a whole line into memory, which can starve memory for a
    /// newline-free giant line.
    #[serde(default)]
    pub read_max_line_bytes: Option<Limit>,
    /// Maximum bytes of an image file the `ReadImage` tool will read. This is a
    /// source-read cap, not a per-request cap. Absent uses the compiled default
    /// of 32 MiB; `"unlimited"` disables the cap.
    #[serde(default)]
    pub read_image_max_bytes: Option<Limit>,
    /// Maximum bytes of hook stdout and stderr captured per hook invocation.
    /// Absent uses the compiled default of 64 KiB; `"unlimited"` disables
    /// truncation.
    #[serde(default)]
    pub hook_output_limit: Option<Limit>,
}

/// TOML-facing per-request image budget overlay.
///
/// Each key is a positive integer. There is deliberately no `"unlimited"`
/// sentinel: an unbounded request image set is the behavior these budgets
/// exist to prevent. An absent key inherits the lower-precedence value, then
/// the compiled default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "the field names mirror the [images] settings keys"
)]
pub struct ImagesSettingsOverlay {
    /// Maximum number of images in one provider request.
    #[serde(default)]
    pub max_images: Option<PositiveLimit>,
    /// Maximum encoded image bytes in one provider request.
    #[serde(default)]
    pub max_bytes: Option<PositiveLimit>,
    /// Maximum decoded pixels across all images in one provider request.
    #[serde(default)]
    pub max_pixels: Option<PositiveLimit>,
}

impl ImagesSettingsOverlay {
    /// Merge present keys from a higher-precedence overlay.
    fn merge(&mut self, incoming: Option<Self>) {
        let Some(incoming) = incoming else {
            return;
        };
        merge_present(&mut self.max_images, incoming.max_images);
        merge_present(&mut self.max_bytes, incoming.max_bytes);
        merge_present(&mut self.max_pixels, incoming.max_pixels);
    }

    /// Apply the compiled defaults and convert every configured value into
    /// the concrete representation used by the request planner.
    #[must_use]
    pub const fn resolve(&self) -> ResolvedImagePolicy {
        ResolvedImagePolicy {
            max_images: match self.max_images {
                Some(limit) => limit.value() as usize,
                None => DEFAULT_IMAGE_MAX_IMAGES as usize,
            },
            max_bytes: match self.max_bytes {
                Some(limit) => limit.value() as usize,
                None => DEFAULT_IMAGE_MAX_BYTES as usize,
            },
            max_pixels: match self.max_pixels {
                Some(limit) => limit.value() as u64,
                None => DEFAULT_IMAGE_MAX_PIXELS as u64,
            },
        }
    }
}

/// Resolved per-request image policy used by the request planner.
///
/// Unlike [`LimitsSettingsOverlay`], this type has no absent-key or sentinel
/// state. It is produced once after every settings layer has been merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[expect(
    clippy::struct_field_names,
    reason = "the field names mirror the [images] settings keys"
)]
pub struct ResolvedImagePolicy {
    /// Maximum number of images in one provider request.
    pub max_images: usize,
    /// Maximum encoded image bytes in one provider request.
    pub max_bytes: usize,
    /// Maximum decoded pixels across all images in one provider request.
    pub max_pixels: u64,
}

/// Compiled default for [`ResolvedImagePolicy::max_images`].
pub const DEFAULT_IMAGE_MAX_IMAGES: u32 = 8;
/// Compiled default for [`ResolvedImagePolicy::max_bytes`]: 16 MiB of encoded
/// image bytes, before the roughly four-thirds base64 expansion.
pub const DEFAULT_IMAGE_MAX_BYTES: u32 = 16 * 1024 * 1024;
/// Compiled default for [`ResolvedImagePolicy::max_pixels`]: 24 megapixels
/// across all images in one request.
pub const DEFAULT_IMAGE_MAX_PIXELS: u32 = 24_000_000;
/// Compiled longest-edge bound for one emitted image. An image above this
/// edge is downscaled before it is placed in a request.
pub const MAX_IMAGE_EDGE: u32 = 2048;

impl ResolvedImagePolicy {
    /// The compiled defaults used when no `[images]` key is configured.
    #[must_use]
    pub const fn defaults() -> Self {
        Self {
            max_images: DEFAULT_IMAGE_MAX_IMAGES as usize,
            max_bytes: DEFAULT_IMAGE_MAX_BYTES as usize,
            max_pixels: DEFAULT_IMAGE_MAX_PIXELS as u64,
        }
    }
}

impl Default for ResolvedImagePolicy {
    fn default() -> Self {
        Self::defaults()
    }
}

/// TOML-facing automatic compaction overlay.
///
/// Each key is optional so a project or profile section overrides only the
/// keys it sets. There is deliberately no `"unlimited"` sentinel: every budget
/// remains a finite positive integer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactionSettingsOverlay {
    /// Whether automatic pre-request compaction runs.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Fallback trigger as a percentage of the model context window.
    #[serde(default)]
    pub threshold_percent: Option<u8>,
    /// Absolute trigger line that replaces the percentage fallback.
    #[serde(default)]
    pub trigger_tokens: Option<PositiveLimit>,
    /// Token target for the retained tail after a checkpoint.
    #[serde(default)]
    pub keep_recent_tokens: Option<PositiveLimit>,
    /// Token target for the generated summary text.
    #[serde(default)]
    pub summary_tokens: Option<PositiveLimit>,
    /// Total wall-clock budget for one summary operation, including retries.
    #[serde(default)]
    pub summary_timeout_secs: Option<PositiveLimit>,
    /// `[[models]]` entry name used for checkpoint summaries.
    #[serde(default)]
    pub model: Option<String>,
    /// Profile whose effective `default_model` selects the summary model when
    /// [`Self::model`] is absent.
    #[serde(default)]
    pub profile: Option<String>,
    /// Reasoning effort override for checkpoint summaries.
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl CompactionSettingsOverlay {
    /// Merge present keys from a higher-precedence overlay.
    fn merge(&mut self, incoming: Option<Self>) {
        let Some(incoming) = incoming else {
            return;
        };
        merge_present(&mut self.enabled, incoming.enabled);
        merge_present(&mut self.threshold_percent, incoming.threshold_percent);
        merge_present(&mut self.trigger_tokens, incoming.trigger_tokens);
        merge_present(&mut self.keep_recent_tokens, incoming.keep_recent_tokens);
        merge_present(&mut self.summary_tokens, incoming.summary_tokens);
        merge_present(
            &mut self.summary_timeout_secs,
            incoming.summary_timeout_secs,
        );
        merge_present(&mut self.model, incoming.model);
        merge_present(&mut self.profile, incoming.profile);
        merge_present(&mut self.reasoning_effort, incoming.reasoning_effort);
    }

    /// Apply the compiled defaults to every absent key.
    ///
    /// Each budget setting has exactly one consumer in the resolved policy:
    /// `enabled` becomes [`CompactionPolicy::enabled`], `threshold_percent`
    /// becomes the fallback trigger percentage, `trigger_tokens` becomes the
    /// absolute trigger and its post-compaction target, `keep_recent_tokens`
    /// becomes the retained-tail target, `summary_tokens` becomes the
    /// summary-text target, and `summary_timeout_secs` becomes the total
    /// summary deadline. The summarizer keys resolve separately through
    /// [`Self::resolve_summarizer`].
    #[must_use]
    pub const fn resolve(&self) -> CompactionPolicy {
        CompactionPolicy {
            enabled: match self.enabled {
                Some(enabled) => enabled,
                None => false,
            },
            threshold_percent: match self.threshold_percent {
                Some(threshold_percent) => threshold_percent,
                None => DEFAULT_THRESHOLD_PERCENT,
            },
            trigger_tokens: Self::optional_tokens(self.trigger_tokens),
            retained_tokens: Self::tokens_or(self.keep_recent_tokens, DEFAULT_RETAINED_TOKENS),
            summary_tokens: Self::tokens_or(self.summary_tokens, DEFAULT_SUMMARY_TOKENS),
            summary_timeout_secs: match self.summary_timeout_secs {
                Some(timeout) => timeout.value() as u64,
                None => DEFAULT_SUMMARY_TIMEOUT_SECS,
            },
        }
    }

    const fn optional_tokens(limit: Option<PositiveLimit>) -> Option<u64> {
        match limit {
            Some(limit) => Some(limit.value() as u64),
            None => None,
        }
    }

    const fn tokens_or(limit: Option<PositiveLimit>, default: u64) -> u64 {
        match limit {
            Some(limit) => limit.value() as u64,
            None => default,
        }
    }

    /// Resolve the optional summarizer selection for checkpoint summaries.
    #[must_use]
    pub fn resolve_summarizer(&self) -> CompactionSummarizerSettings {
        CompactionSummarizerSettings {
            model: self.model.clone(),
            profile: self.profile.clone(),
            reasoning_effort: self.reasoning_effort,
        }
    }
}

/// Model, profile, and effort selection for checkpoint summaries.
///
/// Absent fields fall back to the conversation configuration. An explicit
/// `model` wins over the named profile's effective `default_model`; the named
/// profile contributes only that model selection. `reasoning_effort` overrides
/// the selected model's declared effort, or the conversation effort when no
/// model or profile is selected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CompactionSummarizerSettings {
    /// `[[models]]` entry name used for checkpoint summaries.
    pub model: Option<String>,
    /// Profile whose effective `default_model` selects the summary model.
    pub profile: Option<String>,
    /// Reasoning effort override for checkpoint summaries.
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Default automatic threshold percentage.
pub const DEFAULT_THRESHOLD_PERCENT: u8 = 80;

/// Default retained-tail token target.
pub const DEFAULT_RETAINED_TOKENS: u64 = 20_000;

/// Default summary-text token target.
pub const DEFAULT_SUMMARY_TOKENS: u64 = 4_096;

/// Default total wall-clock budget for one summary operation.
pub const DEFAULT_SUMMARY_TIMEOUT_SECS: u64 = 180;

/// A configured compaction policy before window clamping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CompactionPolicy {
    /// Whether automatic pre-request compaction runs.
    pub enabled: bool,
    /// Fallback trigger as a percentage of the model context window.
    pub threshold_percent: u8,
    /// Absolute trigger line that replaces the percentage fallback.
    pub trigger_tokens: Option<u64>,
    /// Token target for the retained tail after a checkpoint.
    pub retained_tokens: u64,
    /// Token target for the generated summary text.
    pub summary_tokens: u64,
    /// Total wall-clock budget for one summary operation, in seconds.
    pub summary_timeout_secs: u64,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_percent: DEFAULT_THRESHOLD_PERCENT,
            trigger_tokens: None,
            retained_tokens: DEFAULT_RETAINED_TOKENS,
            summary_tokens: DEFAULT_SUMMARY_TOKENS,
            summary_timeout_secs: DEFAULT_SUMMARY_TIMEOUT_SECS,
        }
    }
}

impl CompactionPolicy {
    /// Validate the policy and clamp its budgets to the known window.
    ///
    /// An absolute trigger replaces the percentage-derived line and must be
    /// strictly below a known window. The post-compaction target is three
    /// quarters of the effective trigger. The retained tail is clamped to one
    /// quarter of the window and the summary text to one sixteenth. A zero
    /// effective budget, a threshold outside 10 through 80, or a window that
    /// cannot express a positive trigger and target is rejected.
    ///
    /// # Errors
    ///
    /// Returns an error for any invalid or unsatisfiable policy.
    pub fn validate(self, window: Option<u32>) -> anyhow::Result<ResolvedCompactionPolicy> {
        anyhow::ensure!(
            (10..=80).contains(&self.threshold_percent),
            "compaction threshold_percent must be between 10 and 80, got {}",
            self.threshold_percent
        );
        anyhow::ensure!(
            self.retained_tokens > 0,
            "compaction retained target must be positive"
        );
        anyhow::ensure!(
            self.summary_tokens > 0,
            "compaction summary target must be positive"
        );
        anyhow::ensure!(
            self.summary_timeout_secs > 0,
            "compaction summary_timeout_secs must be positive"
        );
        anyhow::ensure!(
            self.trigger_tokens.is_none_or(|trigger| trigger > 0),
            "compaction trigger_tokens must be positive"
        );
        if let Some(window) = window {
            anyhow::ensure!(window > 0, "compaction window must be positive");
        }

        let trigger_tokens = match (self.trigger_tokens, window) {
            (Some(trigger), Some(window)) => {
                anyhow::ensure!(
                    trigger < u64::from(window),
                    "compaction trigger_tokens {trigger} must be below the {window}-token \
                     context window"
                );
                Some(trigger)
            },
            (Some(trigger), None) => Some(trigger),
            (None, Some(window)) => {
                Some(u64::from(window).saturating_mul(u64::from(self.threshold_percent)) / 100)
            },
            (None, None) => None,
        };
        if let Some(trigger) = trigger_tokens {
            anyhow::ensure!(trigger > 0, "compaction trigger must be positive");
        }
        let target_tokens = trigger_tokens.map(|trigger| trigger.saturating_mul(3) / 4);
        if let Some(target) = target_tokens {
            anyhow::ensure!(
                target > 0,
                "compaction trigger produces a zero post-compaction target"
            );
        }

        let retained_tokens = window.map_or(self.retained_tokens, |window| {
            self.retained_tokens.min(u64::from(window) / 4)
        });
        let summary_tokens = window.map_or(self.summary_tokens, |window| {
            self.summary_tokens.min(u64::from(window) / 16)
        });
        anyhow::ensure!(
            retained_tokens > 0,
            "compaction retained target clamps to zero for a {}-token window",
            window.unwrap_or_default()
        );
        anyhow::ensure!(
            summary_tokens > 0,
            "compaction summary target clamps to zero for a {}-token window",
            window.unwrap_or_default()
        );

        Ok(ResolvedCompactionPolicy {
            enabled: self.enabled,
            threshold_percent: self.threshold_percent,
            retained_tokens,
            summary_tokens,
            summary_timeout: Duration::from_secs(self.summary_timeout_secs),
            trigger_tokens,
            target_tokens,
        })
    }
}

/// A policy after window clamping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCompactionPolicy {
    /// Whether automatic pre-request compaction runs.
    pub enabled: bool,
    /// Fallback trigger percentage retained for diagnostics.
    pub threshold_percent: u8,
    /// Token target for the retained tail after a checkpoint.
    pub retained_tokens: u64,
    /// Token target for the generated summary text.
    pub summary_tokens: u64,
    /// Total wall-clock budget for one summary operation, including retries.
    pub summary_timeout: Duration,
    /// Effective trigger line, when the policy or window supplies one.
    pub trigger_tokens: Option<u64>,
    /// Post-compaction input target.
    pub target_tokens: Option<u64>,
}

/// Resolved runtime limits for the agent loop and tools.
///
/// This type contains no TOML sentinel values or absent-key state. It is
/// produced once after all settings overlays have been merged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ResolvedLimits {
    /// Maximum number of agent-loop turns, or `None` when uncapped.
    pub max_turns: Option<u32>,
    /// Maximum number of executed tool calls, or `None` when uncapped.
    pub max_tool_calls: Option<u32>,
    /// Resolved budgets for Bash, Read, and hooks.
    pub tool_limits: ToolLimits,
}

/// Resolved limits for Bash, Read, Edit, and hook tools.
///
/// Optional byte caps use `None` when the user configures `"unlimited"`.
/// Bash session bounds are mandatory positive values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ToolLimits {
    /// Maximum bytes of Bash output returned inline (shared producer cap).
    pub bash_output_max_bytes: Option<usize>,
    /// Retained for settings compatibility; session-managed Bash commands do
    /// not use it.
    pub bash_read_cap: Option<usize>,
    /// Maximum unread bytes held for one Bash session, clamped to the shared
    /// producer cap.
    pub bash_session_output_max_bytes: usize,
    pub bash_session_max: usize,
    pub bash_session_max_seconds: u64,
    pub bash_session_exited_ttl_seconds: u64,
    /// Default Read window in lines (default 500).
    pub read_default_end_line: Option<usize>,
    /// Maximum bytes of line-aligned Read output before truncation (default
    /// 100,000).
    pub read_max_output_bytes: Option<usize>,
    /// Maximum bytes of an input file Edit will read (default 16 MiB).
    pub edit_max_file_bytes: Option<usize>,
    /// Maximum bytes delivered for a single Read line (default 10,000).
    pub read_max_line_bytes: Option<usize>,
    /// Maximum bytes of an image file `ReadImage` will read (default 32 MiB).
    pub read_image_max_bytes: Option<usize>,
    /// Maximum bytes of hook stdout/stderr captured per hook (default 64 KiB).
    pub hook_output_limit: Option<usize>,
}

/// Compiled defaults for the tool limits, used when the matching `[limits]`
/// key is absent.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the producer cap is a compile-time 2 MiB constant"
)]
pub const DEFAULT_BASH_OUTPUT_MAX_BYTES: u32 = TOOL_OUTPUT_PRODUCER_LIMIT_BYTES as u32;
/// Legacy Bash read-cap default. Session-managed Bash no longer uses it.
pub const DEFAULT_BASH_READ_CAP: u32 = 100_000;
#[expect(
    clippy::cast_possible_truncation,
    reason = "the producer cap is a compile-time 2 MiB constant"
)]
pub const DEFAULT_BASH_SESSION_OUTPUT_MAX_BYTES: u32 = TOOL_OUTPUT_PRODUCER_LIMIT_BYTES as u32;
pub const DEFAULT_BASH_SESSION_MAX: u32 = 16;
pub const DEFAULT_BASH_SESSION_MAX_SECONDS: u32 = 3_600;
pub const DEFAULT_BASH_SESSION_EXITED_TTL_SECONDS: u32 = 600;
/// Compiled default for [`ToolLimits::read_default_end_line`].
pub const DEFAULT_READ_DEFAULT_END_LINE: u32 = 500;
/// Compiled default for [`ToolLimits::read_max_output_bytes`].
pub const DEFAULT_READ_MAX_OUTPUT_BYTES: u32 = 100_000;
/// Compiled default for [`ToolLimits::edit_max_file_bytes`]: 16 MiB, large
/// enough to edit ordinary source and data files while keeping whole-file
/// reads bounded.
pub const DEFAULT_EDIT_MAX_FILE_BYTES: u32 = 16 * 1024 * 1024;
/// Compiled default for [`ToolLimits::read_max_line_bytes`], matching the
/// constant it replaced in `src/clients/tools/read.rs`.
pub const DEFAULT_READ_MAX_LINE_BYTES: u32 = 10_000;
/// Compiled default for [`ToolLimits::read_image_max_bytes`]: 32 MiB.
///
/// This bounds the source file `ReadImage` opens, not the request. A source
/// image within this cap is decoded and downscaled in memory when its longest
/// edge exceeds 2048 pixels; the per-request budgets live in `[images]`.
pub const DEFAULT_READ_IMAGE_MAX_BYTES: u32 = 32 * 1024 * 1024;
/// Compiled default for [`ToolLimits::hook_output_limit`].
pub const DEFAULT_HOOK_OUTPUT_LIMIT: u32 = 64 * 1024;

impl ToolLimits {
    /// The effective Bash/BashSession output cap after applying the shared
    /// producer ceiling.
    #[must_use]
    pub fn bash_output_cap(&self) -> usize {
        self.bash_output_max_bytes
            .map_or(TOOL_OUTPUT_PRODUCER_LIMIT_BYTES, |value| {
                value.min(TOOL_OUTPUT_PRODUCER_LIMIT_BYTES)
            })
    }

    /// The effective unread-output cap for one Bash session.
    #[must_use]
    pub fn bash_session_output_cap(&self) -> usize {
        self.bash_session_output_max_bytes
            .min(TOOL_OUTPUT_PRODUCER_LIMIT_BYTES)
    }

    /// The compiled defaults, matching the constants the `[limits]` keys
    /// replaced. `ToolContext` starts here; configured settings override it.
    pub const fn defaults() -> Self {
        Self {
            bash_output_max_bytes: Some(DEFAULT_BASH_OUTPUT_MAX_BYTES as usize),
            bash_read_cap: Some(DEFAULT_BASH_READ_CAP as usize),
            bash_session_output_max_bytes: DEFAULT_BASH_SESSION_OUTPUT_MAX_BYTES as usize,
            bash_session_max: DEFAULT_BASH_SESSION_MAX as usize,
            bash_session_max_seconds: DEFAULT_BASH_SESSION_MAX_SECONDS as u64,
            bash_session_exited_ttl_seconds: DEFAULT_BASH_SESSION_EXITED_TTL_SECONDS as u64,
            read_default_end_line: Some(DEFAULT_READ_DEFAULT_END_LINE as usize),
            read_max_output_bytes: Some(DEFAULT_READ_MAX_OUTPUT_BYTES as usize),
            edit_max_file_bytes: Some(DEFAULT_EDIT_MAX_FILE_BYTES as usize),
            read_max_line_bytes: Some(DEFAULT_READ_MAX_LINE_BYTES as usize),
            read_image_max_bytes: Some(DEFAULT_READ_IMAGE_MAX_BYTES as usize),
            hook_output_limit: Some(DEFAULT_HOOK_OUTPUT_LIMIT as usize),
        }
    }
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Resolve the merged `[limits]` overlay into runtime values.
impl LimitsSettingsOverlay {
    /// Merge present keys from a higher-precedence overlay.
    fn merge(&mut self, incoming: Option<Self>) {
        let Some(incoming) = incoming else {
            return;
        };
        merge_present(&mut self.max_turns, incoming.max_turns);
        merge_present(&mut self.max_tool_calls, incoming.max_tool_calls);
        merge_present(
            &mut self.bash_output_max_bytes,
            incoming.bash_output_max_bytes,
        );
        merge_present(&mut self.bash_read_cap, incoming.bash_read_cap);
        merge_present(
            &mut self.bash_session_output_max_bytes,
            incoming.bash_session_output_max_bytes,
        );
        merge_present(&mut self.bash_session_max, incoming.bash_session_max);
        merge_present(
            &mut self.bash_session_max_seconds,
            incoming.bash_session_max_seconds,
        );
        merge_present(
            &mut self.bash_session_exited_ttl_seconds,
            incoming.bash_session_exited_ttl_seconds,
        );
        merge_present(
            &mut self.read_default_end_line,
            incoming.read_default_end_line,
        );
        merge_present(
            &mut self.read_max_output_bytes,
            incoming.read_max_output_bytes,
        );
        merge_present(&mut self.edit_max_file_bytes, incoming.edit_max_file_bytes);
        merge_present(&mut self.read_max_line_bytes, incoming.read_max_line_bytes);
        merge_present(
            &mut self.read_image_max_bytes,
            incoming.read_image_max_bytes,
        );
        merge_present(&mut self.hook_output_limit, incoming.hook_output_limit);
    }

    /// Apply the compiled defaults and convert all configured sentinel values
    /// into the concrete representation used by the runtime.
    pub fn resolve(&self) -> ResolvedLimits {
        ResolvedLimits {
            max_turns: self.max_turns.and_then(Limit::value),
            max_tool_calls: self.max_tool_calls.and_then(Limit::value),
            tool_limits: ToolLimits {
                bash_output_max_bytes: resolve_tool_limit(
                    self.bash_output_max_bytes,
                    DEFAULT_BASH_OUTPUT_MAX_BYTES,
                ),
                bash_read_cap: resolve_tool_limit(self.bash_read_cap, DEFAULT_BASH_READ_CAP),
                bash_session_output_max_bytes: self
                    .bash_session_output_max_bytes
                    .map_or(DEFAULT_BASH_SESSION_OUTPUT_MAX_BYTES as usize, |cap| {
                        cap.0 as usize
                    }),
                bash_session_max: self
                    .bash_session_max
                    .map_or(DEFAULT_BASH_SESSION_MAX as usize, |cap| cap.0 as usize),
                bash_session_max_seconds: u64::from(
                    self.bash_session_max_seconds
                        .map_or(DEFAULT_BASH_SESSION_MAX_SECONDS, |cap| cap.0),
                ),
                bash_session_exited_ttl_seconds: u64::from(
                    self.bash_session_exited_ttl_seconds
                        .map_or(DEFAULT_BASH_SESSION_EXITED_TTL_SECONDS, |cap| cap.0),
                ),
                read_default_end_line: resolve_tool_limit(
                    self.read_default_end_line,
                    DEFAULT_READ_DEFAULT_END_LINE,
                ),
                read_max_output_bytes: resolve_tool_limit(
                    self.read_max_output_bytes,
                    DEFAULT_READ_MAX_OUTPUT_BYTES,
                ),
                edit_max_file_bytes: resolve_tool_limit(
                    self.edit_max_file_bytes,
                    DEFAULT_EDIT_MAX_FILE_BYTES,
                ),
                read_max_line_bytes: resolve_tool_limit(
                    self.read_max_line_bytes,
                    DEFAULT_READ_MAX_LINE_BYTES,
                ),
                read_image_max_bytes: resolve_tool_limit(
                    self.read_image_max_bytes,
                    DEFAULT_READ_IMAGE_MAX_BYTES,
                ),
                hook_output_limit: resolve_tool_limit(
                    self.hook_output_limit,
                    DEFAULT_HOOK_OUTPUT_LIMIT,
                ),
            },
        }
    }
}

/// Resolve one output budget, using its compiled default when the key is absent.
fn resolve_tool_limit(limit: Option<Limit>, default: u32) -> Option<usize> {
    limit.map_or(Some(default as usize), |limit| {
        limit.value().map(|value| value as usize)
    })
}

/// Result of loading and merging settings from all sources.
///
/// Contains the merged model map, the resolved default model name,
/// and merged directories.
/// Separate from [`Settings`] to represent the post-merge state.
#[derive(Debug, Clone)]
pub struct LoadedSettings {
    pub gates: crate::config::gates::GateConfiguration,
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
    /// Exact registered tool names selected by settings and profile. `None`
    /// means all tools; an empty list means no tools.
    pub tools_enabled: Option<Vec<String>>,
    /// Resolved system prompt path from settings (global + project + selected profile).
    /// This is a path to a custom prompt file, checked after `.cake/system.md`
    /// but before `~/.config/cake/system.md` in the precedence chain.
    pub system_prompt: Option<String>,
    /// Effective LLM judge settings (global + project merged, defaults applied).
    pub judge: JudgeSettings,
    /// Effective agent-loop and tool output limits after all overlays resolve.
    pub limits: ResolvedLimits,
    /// Effective per-request image budgets after all overlays resolve.
    pub images: ResolvedImagePolicy,
    /// Effective automatic compaction policy after all overlays resolve and
    /// before model-window validation.
    pub compaction: CompactionPolicy,
    /// Configured summarizer selection for automatic compaction.
    pub compaction_summarizer: CompactionSummarizerSettings,
    /// Effective `default_model` for every named profile after profile
    /// overlays, used by `[compaction] profile`.
    pub profile_default_models: HashMap<String, Option<String>>,
    /// Warnings about unrecognized keys in settings files, one per issue and
    /// already formatted with the source file path. Unrecognized keys are
    /// ignored (existing behavior); these warnings surface them instead of
    /// silently dropping them.
    pub warnings: Vec<String>,
}

impl LoadedSettings {
    /// Resolve the separate summarizer model for automatic compaction.
    ///
    /// An explicit `[compaction] model` wins over the named profile's effective
    /// `default_model`; with neither, an effort override clones the conversation
    /// configuration. `None` means the summary call uses the conversation
    /// configuration unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error when the selected model is missing or its credential
    /// cannot be resolved. Name validation normally rejects a missing model at
    /// settings load, so the missing-entry branch is a defensive fallback.
    pub fn resolve_compaction_summarizer(
        &self,
        conversation: &ResolvedModelConfig,
    ) -> anyhow::Result<Option<ResolvedModelConfig>> {
        let selection = &self.compaction_summarizer;
        let model_name = selection.model.as_deref().or_else(|| {
            selection.profile.as_deref().and_then(|profile| {
                self.profile_default_models
                    .get(profile)
                    .and_then(Option::as_deref)
            })
        });
        let Some(model_name) = model_name else {
            let Some(effort) = selection.reasoning_effort else {
                return Ok(None);
            };
            let mut config = conversation.clone();
            config.model_config.reasoning_effort = Some(effort);
            return Ok(Some(config));
        };
        let definition = self.models.get(model_name).ok_or_else(|| {
            anyhow::anyhow!(
                "compaction summarizer model '{model_name}' is not configured in settings.toml"
            )
        })?;
        let mut config = definition.to_model_config();
        if let Some(effort) = selection.reasoning_effort {
            config.reasoning_effort = Some(effort);
        }
        Ok(Some(ResolvedModelConfig::resolve(config)?))
    }

    /// Print the settings warnings to stderr, one per line, prefixed with
    /// `warning:`. The tracing log is file-only, so settings typos surface
    /// here or not at all in the terminal.
    pub fn print_warnings(&self) {
        for warning in &self.warnings {
            eprintln!("warning: {warning}");
        }
    }
}

/// Which settings file supplied a merged value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsOrigin {
    /// User-owned global settings, always trusted.
    Global,
    /// Project `.cake/settings.toml`, subject to the trust decision.
    Project,
}

/// One settings file read exactly once for hashing, parsing, and merging.
#[derive(Debug, Clone)]
pub struct SettingsSource {
    /// Absolute path used in warnings and diagnostics.
    pub path: PathBuf,
    /// Whether this is global or project settings.
    pub origin: SettingsOrigin,
    /// Lowercase hex SHA-256 of the file bytes.
    pub sha256: String,
    /// The parsed settings.
    pub settings: Settings,
    /// Unrecognized-key warnings for this file.
    pub warnings: Vec<String>,
}

impl SettingsSource {
    /// Return a copy with every authority-bearing key removed.
    ///
    /// Used by inspection commands and by enforcement when project authority
    /// is not trusted: the low-authority overlay still applies, but nothing in
    /// the returned value can grant paths, start gates, or read host files.
    #[must_use]
    pub fn without_authority(&self) -> Self {
        let mut source = self.clone();
        source.settings = source.settings.without_authority();
        source
    }
}

/// Settings plus the project trust decision they were loaded under.
#[derive(Debug, Clone)]
pub struct SettingsLoad {
    /// The merged settings.
    pub settings: LoadedSettings,
    /// The project trust decision.
    pub trust: ProjectTrustReport,
    /// Exact project hook file bytes validated with the trust snapshot.
    pub hook_files: Vec<crate::config::trust::HookFileSource>,
    /// Exact `.cake/mcp.json` bytes validated with the same snapshot.
    pub mcp_config_bytes: Option<Vec<u8>>,
}

/// Definition of a named model in settings.toml.
///
/// Each model has a unique name that can be used with `--model <name>`
/// to select a specific model configuration.
///
/// # Examples
///
/// Model definitions are declared in the global `settings.toml` under
/// `[[models]]`; project settings may select one by name. Use
/// [`SettingsLoader::load_with_project_config`] to obtain a `LoadedSettings`
/// map and access definitions by name.
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
    /// Top-k sampling parameter
    #[serde(default)]
    pub top_k: Option<u32>,
    /// Maximum number of output tokens
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// Model context window in input tokens. When set, the agent can compare
    /// accumulated usage against the window and report the remaining budget.
    /// Absent means the window is unknown and cake keeps current behavior.
    #[serde(default)]
    pub context_window: Option<u32>,
    /// Reasoning effort level
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Reasoning-effort levels to expand into selectable variants.
    ///
    /// Each level generates a model named `<name>-<level>` that copies this
    /// definition and sets `reasoning_effort` to that level. The base entry
    /// remains selectable with its own configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_efforts: Option<Vec<ReasoningEffort>>,
    /// Reasoning summary mode (Responses API only)
    #[serde(default)]
    pub reasoning_summary: Option<String>,
    /// Maximum reasoning tokens budget
    #[serde(default)]
    pub reasoning_max_tokens: Option<u32>,
    /// Provider routing hints
    #[serde(default)]
    pub providers: Vec<String>,
    /// Whether this model accepts image input. Gates the `ReadImage` tool so
    /// cake never sends pixels to a text-only model. Defaults to false.
    #[serde(default)]
    pub supports_images: bool,
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
            supports_images: self.supports_images,
            model: self.model.clone(),
            api_type: self.api_type,
            base_url: self.base_url.clone(),
            api_key_env: self.api_key_env.clone(),
            provider: self.provider,
            provider_headers: self.provider_headers.clone(),
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            max_output_tokens: self.max_output_tokens,
            context_window: self.context_window,
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

    #[error("Invalid model configuration '{name}': {reason}")]
    InvalidModelBaseUrl { name: String, reason: String },

    #[error("Invalid model configuration '{name}': {reason}")]
    InvalidModelApiKeyEnv { name: String, reason: String },

    #[error(
        "Model '{name}' targets the ChatGPT Codex backend, which cake no longer supports. \
         Remove the [[models]] entry or point base_url at an OpenAI-compatible endpoint."
    )]
    RetiredModelBackend { name: String },

    #[error("Duplicate model name '{name}' in settings")]
    DuplicateModelName { name: String },

    #[error("Model '{name}' lists reasoning effort '{level}' more than once in reasoning_efforts")]
    DuplicateReasoningEffort { name: String, level: String },

    #[error(
        "Model '{base}' generates model '{name}' from reasoning_efforts, but '{name}' is also \
         defined in the same settings file"
    )]
    GeneratedModelNameConflict { base: String, name: String },

    #[error("Invalid profile name '{name}': {reason}")]
    InvalidProfileName { name: String, reason: String },

    #[error("Profile '{name}' defines models, but model configs are only supported at top level")]
    ProfileModelsUnsupported { name: String },

    #[error(
        "Project settings {path} define models [{}], but model definitions are global-only. \
         Move the entries to <config>/cake/settings.toml and select one by name from the project.",
        names.join(", ")
    )]
    ProjectModelsUnsupported { path: PathBuf, names: Vec<String> },

    #[error(
        "Project settings {path} broaden [tools].enabled with [{}], but a project may only \
         narrow the inherited tool selection.",
        added.join(", ")
    )]
    ProjectToolsBroadened { path: PathBuf, added: Vec<String> },

    #[error("Unknown profile '{name}'{available}. Define [profiles.{name}] in settings.toml.")]
    UnknownProfile { name: String, available: String },

    #[error(
        "Default model '{name}' not found in models list. \
         Define a [[models]] entry with name = \"{name}\", \
         or change default_model to an existing model name."
    )]
    DefaultModelNotFound { name: String },

    #[error(
        "Compaction summarizer model '{name}' not found in models list. \
         [compaction] model and the named profile's default_model take a [[models]] \
         name, like default_model; define a [[models]] entry with name = \"{name}\" or \
         remove the summarizer setting."
    )]
    CompactionModelNotFound { name: String },

    #[error(
        "Judge model '{name}' not found in models list. \
         [tools.bash.judge] model takes a [[models]] name, like default_model; \
         define a [[models]] entry with name = \"{name}\", or remove the \
         setting to use the agent's configured model."
    )]
    JudgeModelNotFound { name: String },

    #[error("Invalid gate configuration: {message}")]
    InvalidGateConfiguration { message: String },

    #[error("Failed to parse settings file: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("Failed to serialize settings for validation: {0}")]
    SerializeError(#[from] toml::ser::Error),

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
/// let load = SettingsLoader::load_with_project_config(
///     Path::new("/project"),
///     None,
///     false,
///     &crate::config::config_dir(),
///     ProjectConfigMode::Enforce,
/// )?;
/// if let Some(model) = load.settings.models.get("zen") {
///     println!("Model: {}", model.model);
/// }
/// ```
pub struct SettingsLoader;

impl SettingsLoader {
    /// Read one settings file and parse the same bytes that are hashed.
    ///
    /// Returns `Ok(None)` when the path does not exist.
    ///
    /// # Errors
    ///
    /// Returns a settings error when the file cannot be read, is not UTF-8, or
    /// cannot be parsed as TOML.
    pub fn read_source(
        path: &Path,
        origin: SettingsOrigin,
    ) -> Result<Option<SettingsSource>, SettingsError> {
        match std::fs::read(path) {
            Ok(bytes) => Self::read_source_bytes(path, origin, &bytes).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(SettingsError::IoError(error)),
        }
    }

    /// Parse settings from the exact bytes the caller hashed.
    ///
    /// # Errors
    ///
    /// Returns a settings error when the bytes are not UTF-8 or not TOML.
    pub(crate) fn read_source_bytes(
        path: &Path,
        origin: SettingsOrigin,
        bytes: &[u8],
    ) -> Result<SettingsSource, SettingsError> {
        let content = std::str::from_utf8(bytes)
            .map_err(|error| {
                SettingsError::IoError(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            })?
            .to_string();
        let original: toml::Value = toml::from_str(&content)?;
        let settings: Settings = toml::from_str(&content)?;
        let round_trip = toml::Value::try_from(&settings)?;
        let warnings = collect_unknown_keys(&original, &round_trip, path);
        Ok(SettingsSource {
            path: path.to_path_buf(),
            origin,
            sha256: hex::encode(Sha256::digest(bytes)),
            settings,
            warnings,
        })
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

    /// Resolve tool selection from CLI flags and settings.
    ///
    /// Precedence (highest to lowest):
    /// 1. `--no-tools` CLI flag
    /// 2. `--tools name1,name2` CLI flag
    /// 3. `tools.enabled` in resolved settings (profile over top-level)
    /// 4. Default: no restriction, exposing all available tools
    ///
    /// `None` means no CLI override, so the resolved settings value is kept;
    /// `Some(vec![])` means no tools. CLI values replace lower-precedence
    /// settings rather than unioning with them.
    pub fn resolve_tools_config(
        no_tools: bool,
        tools_flag: Option<&str>,
        settings: Option<&[String]>,
    ) -> Option<Vec<String>> {
        if no_tools {
            return Some(Vec::new());
        }

        if let Some(names) = tools_flag {
            let tool_names: Vec<String> = names
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            return Some(tool_names);
        }

        settings.map(<[String]>::to_vec)
    }

    /// Loads global and project settings with the project trust decision.
    ///
    /// The global source is always trusted. The project source is read once by
    /// [`ProjectConfigSnapshot`], merged for low-authority keys, and merged in
    /// full only when the trust decision allows project authority.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigLoadError::Settings`] for an invalid settings file and
    /// [`ConfigLoadError::Trust`] when an enforcing mode cannot use the project
    /// configuration.
    pub fn load_with_project_config(
        project_dir: &Path,
        profile: Option<&str>,
        no_gate: bool,
        config_dir: &Path,
        mode: ProjectConfigMode,
    ) -> Result<SettingsLoad, ConfigLoadError> {
        let mut acc = SettingsAccumulator::default();
        Self::merge_global_source(&mut acc, config_dir)?;
        let snapshot = ProjectConfigSnapshot::load(project_dir, config_dir, mode)?;
        let trust = snapshot.evaluate()?;
        Self::merge_project_source(&mut acc, &snapshot, mode, &trust)?;
        let settings = Self::finish_loaded(acc, profile, no_gate)?;
        Ok(SettingsLoad {
            settings,
            trust,
            hook_files: snapshot.hook_files.clone(),
            mcp_config_bytes: snapshot.mcp_bytes.clone(),
        })
    }

    /// Merge the user-owned global settings source.
    fn merge_global_source(
        acc: &mut SettingsAccumulator,
        config_dir: &Path,
    ) -> Result<(), SettingsError> {
        let global_path = config_dir.join("cake").join("settings.toml");
        if let Some(source) = Self::read_source(&global_path, SettingsOrigin::Global)? {
            Self::merge_source(&source, acc)?;
        }
        Ok(())
    }

    /// Merge the project settings source, stripping authority when the report
    /// does not allow it.
    fn merge_project_source(
        acc: &mut SettingsAccumulator,
        snapshot: &ProjectConfigSnapshot,
        mode: ProjectConfigMode,
        trust: &ProjectTrustReport,
    ) -> Result<(), SettingsError> {
        if let Some(source) = &snapshot.settings {
            let applies_authority = mode.is_enforcing() || trust.allows_authority();
            let source = if applies_authority {
                source.clone()
            } else {
                source.without_authority()
            };
            Self::merge_source(&source, acc)?;
        }
        acc.warnings.extend(snapshot.warnings.iter().cloned());
        Ok(())
    }

    /// Apply the selected profile and validate the resolved model and gates.
    fn finish_loaded(
        mut acc: SettingsAccumulator,
        profile: Option<&str>,
        no_gate: bool,
    ) -> Result<LoadedSettings, SettingsError> {
        Self::apply_profile(profile, &mut acc)?;
        Self::validate_default_model(&acc)?;
        Self::validate_compaction_summarizer(&acc)?;
        Self::validate_judge_model(&acc, no_gate)?;
        Self::validate_gate_configuration(&acc, no_gate)?;
        Ok(acc.into_loaded())
    }

    fn merge_source(
        source: &SettingsSource,
        acc: &mut SettingsAccumulator,
    ) -> Result<(), SettingsError> {
        Self::validate_profiles(&source.settings.profiles)?;
        if source.origin == SettingsOrigin::Project && !source.settings.models.is_empty() {
            return Err(SettingsError::ProjectModelsUnsupported {
                path: source.path.clone(),
                names: source
                    .settings
                    .models
                    .iter()
                    .map(|model| model.name.clone())
                    .collect(),
            });
        }
        Self::merge_settings(&source.settings, source.origin, &source.path, acc)?;
        acc.warnings.extend(source.warnings.iter().cloned());
        Ok(())
    }

    fn apply_profile(
        profile: Option<&str>,
        acc: &mut SettingsAccumulator,
    ) -> Result<(), SettingsError> {
        let Some(name) = profile else {
            return Ok(());
        };
        Self::validate_profile_name(name)?;
        let overlays = Self::profile_overlays(name, acc)?;
        for overlay in overlays {
            acc.apply_profile_overlay(&overlay)?;
        }
        Ok(())
    }

    fn validate_profile_name(name: &str) -> Result<(), SettingsError> {
        ModelDefinition::validate_name(name).map_err(|e| SettingsError::InvalidProfileName {
            name: name.to_string(),
            reason: e.to_string(),
        })
    }

    fn profile_overlays(
        name: &str,
        acc: &SettingsAccumulator,
    ) -> Result<Vec<ProfileOverlay>, SettingsError> {
        acc.profiles.get(name).cloned().ok_or_else(|| {
            let mut available: Vec<_> = acc.profiles.keys().cloned().collect();
            available.sort();
            let available = if available.is_empty() {
                String::new()
            } else {
                format!(". Available profiles: {}", available.join(", "))
            };
            SettingsError::UnknownProfile {
                name: name.to_string(),
                available,
            }
        })
    }

    fn validate_default_model(acc: &SettingsAccumulator) -> Result<(), SettingsError> {
        if let Some(ref name) = acc.default_model
            && !acc.models.contains_key(name.as_str())
        {
            return Err(SettingsError::DefaultModelNotFound { name: name.clone() });
        }
        Ok(())
    }

    /// Validate the optional `[compaction]` summarizer selection.
    ///
    /// `model` and a named profile's effective `default_model` must resolve to
    /// a `[[models]]` entry, and `profile` must name a configured profile.
    /// Validation runs even while automatic compaction is disabled so a typo
    /// surfaces before the user enables the feature.
    fn validate_compaction_summarizer(acc: &SettingsAccumulator) -> Result<(), SettingsError> {
        if let Some(name) = acc.compaction.model.as_deref() {
            Self::validate_summarizer_model_name(name, acc)?;
        }
        let Some(profile) = acc.compaction.profile.as_deref() else {
            return Ok(());
        };
        Self::validate_profile_name(profile)?;
        let overlays = Self::profile_overlays(profile, acc)?;
        if let Some(name) = overlays
            .iter()
            .rev()
            .find_map(|overlay| overlay.settings.default_model.as_deref())
        {
            Self::validate_summarizer_model_name(name, acc)?;
        }
        Ok(())
    }

    fn validate_summarizer_model_name(
        name: &str,
        acc: &SettingsAccumulator,
    ) -> Result<(), SettingsError> {
        if acc.models.contains_key(name) {
            return Ok(());
        }
        Err(SettingsError::CompactionModelNotFound {
            name: name.to_string(),
        })
    }

    /// Validate that the judge model (if set) refers to an existing model,
    /// mirroring `default_model`: a `[[models]]` name, resolved to that entry's
    /// full configuration. A bypassed judge's unused model config is inert, so
    /// validation is skipped: `--no-gate`, `CAKE_JUDGE=off`, or
    /// `enabled = false` stays the recovery path even for a misconfigured
    /// judge.
    fn validate_judge_model(acc: &SettingsAccumulator, no_gate: bool) -> Result<(), SettingsError> {
        if acc.gates.selection.is_none()
            && !no_gate
            && !Self::judge_is_bypassed(acc)
            && let Some(ref name) = acc.judge.model
            && !acc.models.contains_key(name.as_str())
        {
            return Err(SettingsError::JudgeModelNotFound { name: name.clone() });
        }
        Ok(())
    }

    /// Validate the explicit `[tools.bash].gates` selection. An explicit
    /// `--no-gate` or environment bypass skips structural validation so the
    /// escape hatch can load a broken configuration; a legacy `enabled =
    /// false` with no explicit selection is already represented as a bypass by
    /// the resolver.
    fn validate_gate_configuration(
        acc: &SettingsAccumulator,
        no_gate: bool,
    ) -> Result<(), SettingsError> {
        if no_gate || crate::config::gates::environment_bypass() {
            return Ok(());
        }
        acc.gates
            .validate()
            .map_err(|message| SettingsError::InvalidGateConfiguration { message })
    }

    /// Whether the judge is bypassed at settings load: `enabled = false` or
    /// the `CAKE_JUDGE=off` environment variable.
    fn judge_is_bypassed(acc: &SettingsAccumulator) -> bool {
        acc.judge.enabled == Some(false) || crate::config::gates::environment_bypass()
    }

    fn merge_settings(
        settings: &Settings,
        origin: SettingsOrigin,
        source_path: &Path,
        acc: &mut SettingsAccumulator,
    ) -> Result<(), SettingsError> {
        Self::add_models_to_map(&mut acc.models, settings.models.clone())?;
        acc.gates.merge(settings.gates.clone());
        if settings.default_model.is_some() {
            acc.default_model.clone_from(&settings.default_model);
        }
        for dir in &settings.directories {
            acc.directories.insert(expand_home_str(dir));
        }
        let sandbox = settings.sandbox.clone().unwrap_or_default();
        acc.sandbox_read_only
            .extend(sandbox.read_only.iter().map(|p| expand_home_str(p)));
        acc.sandbox_writable
            .extend(sandbox.writable.iter().map(|p| expand_home_str(p)));
        if let Some(settings_skills) = &settings.skills {
            acc.skills = settings_skills.clone();
        }
        if settings.system_prompt.is_some() {
            acc.system_prompt.clone_from(&settings.system_prompt);
        }
        Self::merge_tools_settings(settings.tools.clone(), origin, source_path, acc)?;
        acc.limits.merge(settings.limits.clone());
        acc.images.merge(settings.images.clone());
        acc.compaction.merge(settings.compaction.clone());
        for (name, profile) in &settings.profiles {
            acc.profiles
                .entry(name.clone())
                .or_default()
                .push(ProfileOverlay {
                    settings: profile.clone(),
                    origin,
                    path: source_path.to_path_buf(),
                });
        }
        Ok(())
    }

    /// Merge the `[tools]` fields into the accumulator.
    ///
    /// The enabled list replaces lower-precedence selections when present. A
    /// global selection may name anything; a project selection may only narrow
    /// the inherited list.
    fn merge_tools_settings(
        tools: Option<ToolsSettings>,
        origin: SettingsOrigin,
        source_path: &Path,
        acc: &mut SettingsAccumulator,
    ) -> Result<(), SettingsError> {
        let Some(ToolsSettings { enabled, bash }) = tools else {
            return Ok(());
        };
        if origin == SettingsOrigin::Project {
            Self::validate_project_tool_narrowing(
                enabled.as_deref(),
                source_path,
                acc.tools_enabled.as_deref(),
            )?;
        }
        if enabled.is_some() {
            acc.tools_enabled = enabled;
        }
        if let Some(bash) = &bash {
            if bash.gates.is_some() {
                acc.gates.selection.clone_from(&bash.gates);
            }
            acc.gates.legacy_present |= bash.judge.is_some();
        }
        if let Some(judge) = bash.and_then(|b| b.judge) {
            acc.judge.merge(judge);
        }
        Ok(())
    }

    /// Reject a project tool selection that names a tool the inherited list
    /// does not contain. An absent inherited selection exposes every tool, so
    /// any list narrows it.
    fn validate_project_tool_narrowing(
        requested: Option<&[String]>,
        source_path: &Path,
        inherited: Option<&[String]>,
    ) -> Result<(), SettingsError> {
        let (Some(requested), Some(inherited)) = (requested, inherited) else {
            return Ok(());
        };
        let added: Vec<String> = requested
            .iter()
            .filter(|name| !inherited.iter().any(|existing| existing == *name))
            .cloned()
            .collect();
        if added.is_empty() {
            return Ok(());
        }
        Err(SettingsError::ProjectToolsBroadened {
            path: source_path.to_path_buf(),
            added,
        })
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
        let definitions = Self::expand_model_variants(definitions)?;

        let mut seen: HashSet<&str> = HashSet::new();
        for def in &definitions {
            if let Err(e) = ModelDefinition::validate_name(&def.name) {
                return Err(SettingsError::InvalidModelName {
                    name: def.name.clone(),
                    reason: e.to_string(),
                });
            }

            if let Err(reason) = crate::config::model::canonicalize_base_url(&def.base_url) {
                return Err(SettingsError::InvalidModelBaseUrl {
                    name: def.name.clone(),
                    reason,
                });
            }

            if crate::config::model::is_retired_codex_backend(&def.base_url) {
                return Err(SettingsError::RetiredModelBackend {
                    name: def.name.clone(),
                });
            }

            if !is_valid_api_key_env(&def.api_key_env) {
                return Err(SettingsError::InvalidModelApiKeyEnv {
                    name: def.name.clone(),
                    reason: "api_key_env must name an environment variable (ASCII letters, \
                             digits, and underscores, not starting with a digit)"
                        .to_string(),
                });
            }

            if !seen.insert(def.name.as_str()) {
                return Err(SettingsError::DuplicateModelName {
                    name: def.name.clone(),
                });
            }
        }

        for def in definitions {
            let name = def.name.clone();
            map.insert(name, def);
        }

        Ok(())
    }

    /// Expand `reasoning_efforts` into selectable variants.
    ///
    /// Each listed level produces `<name>-<level>` with `reasoning_effort` set
    /// to the level and the generator field cleared. The base entry keeps its
    /// `reasoning_efforts` so `debug models` shows what it generated.
    fn expand_model_variants(
        definitions: Vec<ModelDefinition>,
    ) -> Result<Vec<ModelDefinition>, SettingsError> {
        let explicit_names: HashSet<String> =
            definitions.iter().map(|def| def.name.clone()).collect();
        let mut expanded = Vec::with_capacity(definitions.len());
        for def in definitions {
            let Some(levels) = def.reasoning_efforts.clone() else {
                expanded.push(def);
                continue;
            };
            let template = def.clone();
            expanded.push(def);
            let mut seen = HashSet::new();
            for level in levels {
                if !seen.insert(level.as_str()) {
                    return Err(SettingsError::DuplicateReasoningEffort {
                        name: template.name,
                        level: level.as_str().to_string(),
                    });
                }
                let name = format!("{}-{}", template.name, level.as_str());
                if explicit_names.contains(&name) {
                    return Err(SettingsError::GeneratedModelNameConflict {
                        base: template.name,
                        name,
                    });
                }
                let mut variant = template.clone();
                variant.name = name;
                variant.reasoning_effort = Some(level);
                variant.reasoning_efforts = None;
                expanded.push(variant);
            }
        }
        Ok(expanded)
    }
}

/// Whether `name` is a conventional environment variable name.
///
/// This rejects a pasted credential or arbitrary text without validating that
/// the variable exists. The caller never includes the rejected value in the
/// error.
fn is_valid_api_key_env(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// One profile overlay plus the file and trust class it came from.
#[derive(Debug, Clone)]
struct ProfileOverlay {
    settings: ProfileSettings,
    origin: SettingsOrigin,
    path: PathBuf,
}

/// Judge overlay accumulated across global, project, and profile settings.
#[derive(Default)]
struct JudgeAccumulator {
    model: Option<String>,
    timeout_secs: Option<u64>,
    retry_budget_secs: Option<u64>,
    rubric_file: Option<PathBuf>,
    enabled: Option<bool>,
    allowlist: Vec<String>,
    typesafe: TypeSafeSettings,
}

impl JudgeAccumulator {
    /// Merge a present judge overlay, preserving absent fields and unioning
    /// the allowlist.
    fn merge(&mut self, judge: BashJudgeSettings) {
        let BashJudgeSettings {
            model,
            timeout_secs,
            retry_budget_secs,
            rubric_file,
            enabled,
            allowlist,
            typesafe,
        } = judge;
        merge_present(&mut self.model, model);
        merge_present(&mut self.timeout_secs, timeout_secs);
        merge_present(&mut self.retry_budget_secs, retry_budget_secs);
        merge_present(&mut self.rubric_file, rubric_file);
        merge_present(&mut self.enabled, enabled);
        self.allowlist.extend(allowlist.unwrap_or_default());
        if let Some(typesafe) = typesafe {
            typesafe.apply_to(&mut self.typesafe);
        }
    }

    /// Apply defaults to the accumulated judge settings.
    fn resolve(self) -> JudgeSettings {
        JudgeSettings {
            model: self.model,
            timeout_secs: self
                .timeout_secs
                .unwrap_or(DEFAULT_JUDGE_TIMEOUT_SECS)
                .max(JUDGE_TIMEOUT_MIN_SECS),
            retry_budget_secs: self
                .retry_budget_secs
                .unwrap_or(DEFAULT_JUDGE_RETRY_BUDGET_SECS),
            rubric_file: self.rubric_file,
            enabled: self.enabled.unwrap_or(true),
            allowlist: self.allowlist,
            typesafe: self.typesafe,
        }
    }
}

/// Apply a present overlay value, preserving the target when the key is absent.
fn merge_present<T>(target: &mut Option<T>, incoming: Option<T>) {
    if incoming.is_some() {
        *target = incoming;
    }
}

/// Mutable merge state shared by [`SettingsLoader`] while combining global,
/// project, and profile settings.
#[derive(Default)]
struct SettingsAccumulator {
    gates: crate::config::gates::GateConfiguration,
    models: HashMap<String, ModelDefinition>,
    default_model: Option<String>,
    directories: BTreeSet<String>,
    sandbox_read_only: BTreeSet<String>,
    sandbox_writable: BTreeSet<String>,
    skills: SkillSettings,
    tools_enabled: Option<Vec<String>>,
    profiles: HashMap<String, Vec<ProfileOverlay>>,
    system_prompt: Option<String>,
    judge: JudgeAccumulator,
    limits: LimitsSettingsOverlay,
    images: ImagesSettingsOverlay,
    compaction: CompactionSettingsOverlay,
    warnings: Vec<String>,
}

impl SettingsAccumulator {
    /// Union a profile overlay into the accumulated settings.
    fn apply_profile_overlay(&mut self, overlay: &ProfileOverlay) -> Result<(), SettingsError> {
        let settings = &overlay.settings;
        self.gates.merge(settings.gates.clone());
        if settings
            .tools
            .bash
            .as_ref()
            .is_some_and(|bash| bash.judge.is_some())
        {
            self.gates.legacy_present = true;
        }
        if let Some(bash) = &settings.tools.bash
            && let Some(gates) = &bash.gates
        {
            self.gates.selection = Some(gates.clone());
        }
        if let Some(ref profile_default) = settings.default_model {
            self.default_model = Some(profile_default.clone());
        }
        self.system_prompt = settings
            .system_prompt
            .clone()
            .or_else(|| self.system_prompt.clone());
        self.directories
            .extend(settings.directories.iter().map(|d| expand_home_str(d)));
        if let Some(ref overlay_sandbox) = settings.sandbox {
            self.sandbox_read_only
                .extend(overlay_sandbox.read_only.iter().map(|p| expand_home_str(p)));
            self.sandbox_writable
                .extend(overlay_sandbox.writable.iter().map(|p| expand_home_str(p)));
        }
        self.skills.apply_overlay(settings.skills.clone());
        self.apply_tools_overlay(&settings.tools, overlay)?;
        self.limits.merge(settings.limits.clone());
        self.images.merge(settings.images.clone());
        self.compaction.merge(settings.compaction.clone());
        Ok(())
    }

    fn apply_tools_overlay(
        &mut self,
        overlay: &ToolsSettingsOverlay,
        source: &ProfileOverlay,
    ) -> Result<(), SettingsError> {
        if source.origin == SettingsOrigin::Project {
            SettingsLoader::validate_project_tool_narrowing(
                overlay.enabled.as_deref(),
                &source.path,
                self.tools_enabled.as_deref(),
            )?;
        }
        if overlay.enabled.is_some() {
            self.tools_enabled.clone_from(&overlay.enabled);
        }
        let profile_typesafe = overlay
            .bash
            .as_ref()
            .and_then(|bash| bash.judge.as_ref())
            .and_then(|judge| judge.typesafe.clone());
        if let Some(typesafe) = profile_typesafe {
            typesafe.apply_to(&mut self.judge.typesafe);
        }
        Ok(())
    }

    /// Convert the accumulated merge state into the final [`LoadedSettings`].
    fn into_loaded(self) -> LoadedSettings {
        let profile_default_models = self
            .profiles
            .iter()
            .map(|(name, overlays)| {
                (
                    name.clone(),
                    overlays
                        .iter()
                        .rev()
                        .find_map(|overlay| overlay.settings.default_model.clone()),
                )
            })
            .collect();
        let compaction_summarizer = self.compaction.resolve_summarizer();
        LoadedSettings {
            gates: self.gates,
            models: self.models,
            default_model: self.default_model,
            directories: self.directories.into_iter().collect(),
            sandbox: SandboxSettings {
                read_only: self.sandbox_read_only.into_iter().collect(),
                writable: self.sandbox_writable.into_iter().collect(),
            },
            skills: self.skills,
            tools_enabled: self.tools_enabled,
            system_prompt: self.system_prompt,
            judge: self.judge.resolve(),
            limits: self.limits.resolve(),
            images: self.images.resolve(),
            compaction: self.compaction.resolve(),
            compaction_summarizer,
            profile_default_models,
            warnings: self.warnings,
        }
    }
}

/// Settings keys the deserializer silently dropped, one formatted warning per
/// key. Each warning names the source file and the section, for example
/// `[[models]] entry 'zen'`.
fn collect_unknown_keys(
    original: &toml::Value,
    round_trip: &toml::Value,
    file: &Path,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut path: Vec<String> = Vec::new();
    find_unknown_keys(original, round_trip, &mut path, file, &mut out);
    out
}

/// Walk `original` alongside its serde round-trip. A key present in the
/// original document but absent from the round-trip is unrecognized: serde
/// dropped it during deserialization, so no struct field consumes it.
///
/// `path` holds the table and array keys leading to the current position so
/// reports name the exact location.
fn find_unknown_keys(
    original: &toml::Value,
    round_trip: &toml::Value,
    path: &mut Vec<String>,
    file: &Path,
    out: &mut Vec<String>,
) {
    match (original, round_trip) {
        (toml::Value::Table(original), toml::Value::Table(round_trip)) => {
            for (key, value) in original {
                if let Some(round_trip_value) = round_trip.get(key) {
                    path.push(key.clone());
                    find_unknown_keys(value, round_trip_value, path, file, out);
                    path.pop();
                } else {
                    out.push(format_unknown_key(file, key, path));
                }
            }
        },
        (toml::Value::Array(original), toml::Value::Array(round_trip)) => {
            for (index, (value, round_trip_value)) in
                original.iter().zip(round_trip.iter()).enumerate()
            {
                let label = value
                    .as_table()
                    .and_then(|table| table.get("name"))
                    .and_then(toml::Value::as_str)
                    .map_or_else(|| index.to_string(), str::to_string);
                path.push(label);
                find_unknown_keys(value, round_trip_value, path, file, out);
                path.pop();
            }
        },
        _ => {},
    }
}

/// Format one unrecognized key as a user-facing warning.
fn format_unknown_key(file: &Path, key: &str, path: &[String]) -> String {
    format!(
        "{}: unknown key '{key}' in {}; ignored",
        file.display(),
        section_label(path)
    )
}

/// Human-readable location for a settings path, for example
/// `[[models]] entry 'zen'` or `[tools.bash.judge]`.
fn section_label(path: &[String]) -> String {
    let Some((first, rest)) = path.split_first() else {
        return "top-level settings".to_string();
    };
    if first == "models" {
        let entry = rest.first().map_or("?", String::as_str);
        return match rest.len() {
            1 => format!("[[models]] entry '{entry}'"),
            _ => format!("[[models]] entry '{entry}'.{}", rest[1..].join(".")),
        };
    }
    format!("[{}]", path.join("."))
}

/// Expand `~` in a settings path string, returning the expanded string.
fn expand_home_str(path: &str) -> String {
    expand_home(PathBuf::from(path))
        .to_string_lossy()
        .into_owned()
}
