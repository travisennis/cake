//! cake - AI coding assistant CLI

mod auth;
mod build_info;
mod cli;
mod clients;
mod concurrency;
mod config;
mod exit_code;
mod gates;
mod hooks;
mod logger;
mod program_status;
mod prompts;
mod session_telemetry;
mod time_format;
mod types;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::cli::{
    CliOutputSink, CmdRunner, CommandRunOptions, Commands, RunMode, SessionPersistencePlan,
    TurnResult, execute_persistence_plan, prepare_user_skills_dir,
};
use crate::clients::resolve_linked_worktree_dirs;
use crate::clients::{Agent, SandboxPolicy, ToolContext, resolve_sandbox_policy};
use crate::config::ResolvedModelConfig;
use crate::config::settings::LoadedSettings;
use crate::config::trust::{
    HookFileSource, ProjectConfigMode, ProjectConfigSnapshot, ProjectConfigUse, ProjectTrustReport,
};
use crate::config::{
    AgentsFile, DataDir, DiagnosticLevel, HookSource, HooksLoader, ModelConfig, ModelDefinition,
    ReasoningEffort, Session, SessionResumeConfig, SettingsLoader, SkillCatalog, discover_skills,
    discover_skills_with_paths, parse_skill_path_list, read_agents_files, user_skills_dir,
    worktree,
};
use crate::hooks::{HookContext, HookRunner, SessionEndReason, StopHookOutcome};
use crate::program_status::{BlockedKind, State};

use crate::session_telemetry::{SessionTelemetryRecord, SessionTelemetryWriter};

use crate::types::{
    CutOffError, LimitExceededError, SessionRecord, SessionRunConfig, StreamRecord, TaskOutcome,
};

use clap::{ArgGroup, Parser, ValueEnum};

use serde::Serialize;
#[cfg(unix)]
use tokio::signal::unix::Signal;
use tracing::info;

/// Output format for the response
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// Plain text output
    #[default]
    Text,
    /// Stream each message as JSON as it's received
    StreamJson,
    /// Output a single JSON object with result metadata at completion
    Json,
}

/// AI coding assistant CLI
#[derive(Clone, Parser)]
#[command(author, version, about, long_about = None)]
#[command(after_help = "\
Examples:
  cake \"summarize the uncommitted changes\"
  cake --output-format json \"summarize the failing build\" | jq -r '.result'
  cake --model zen --sandbox read-only \"explain src/main.rs\"
  cake sessions list --json | jq -r '.data.sessions[0].session_id'
  cake debug config --json | jq '.data.model'
  cake debug session 550e8400-e29b-41d4-a716-446655440000 --json | jq '.data.tasks'
  cake debug blobs --json | jq '.data.blobs.totals'
  cake debug models --json | jq -r '.data.models[].name'
  cake debug skills --json | jq -r '.data.skills[].name'
  cake debug grants --json | jq '.data.read_write[]'
  cake doctor --json | jq '.checks'
  cake debug tools --json | jq '.data.builtins[] | select(.status == \"filtered\")'

`cake <COMMAND> --help` documents the diagnostic commands and their machine-readable output.")]
#[command(group(
    ArgGroup::new("session_mode")
        .args(["resume", "fork", "no_session"])
        .multiple(false)
))]
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags are inherently boolean"
)]
pub(crate) struct CodingAssistant {
    /// The prompt to send to the AI (use `-` to read from stdin)
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Sets the max tokens value
    #[arg(long)]
    pub max_tokens: Option<u32>,

    /// Output format for the response (text, json, or stream-json)
    #[arg(long, value_enum, default_value = "text")]
    pub output_format: OutputFormat,

    /// Exit nonzero when the run fails
    ///
    /// Only meaningful for `--output-format stream-json`, which reports a
    /// failed task in its final `task_complete` record and exits `0` by
    /// default. With this flag the exit code carries the failure class: `1`
    /// for an agent, tool, or limit failure, `2` for an authentication,
    /// rate-limit, or network failure, and `3` for an input error. A consumer
    /// that closes stdout early still exits `0`, and an interruption still
    /// exits `130`. Text and JSON output already exit nonzero on failure, so
    /// this flag changes nothing there.
    #[arg(long)]
    pub fail_on_error: bool,

    /// Constrain the final response to a JSON document valid against
    /// this JSON Schema file (draft 2020-12). Only the final response
    /// is constrained; tool use and intermediate output are unchanged.
    #[arg(long, value_name = "PATH")]
    pub output_schema: Option<String>,

    /// Resume a specific session by UUID.
    #[arg(long, value_name = "UUID")]
    pub resume: Option<String>,

    /// Resume a session without persisting this invocation
    ///
    /// Appends no session records, writes no telemetry, and does not run hooks;
    /// hooks can inject Developer context and have side effects outside the
    /// session. The session file, its blobs, and the original transcript are
    /// left untouched. Requires `--resume`. Tool execution is unchanged; pair
    /// with `--tool-choice none` for a generation-only call.
    #[arg(long)]
    pub read_only: bool,

    /// Fork a session: copy its history into a new session with a fresh ID.
    /// Use without a value to fork the latest session, or provide a UUID.
    #[arg(long, num_args = 0..=1, default_missing_value = "", value_name = "UUID")]
    pub fork: Option<String>,

    /// Accept a changed provider identity when resuming or forking.
    ///
    /// The stored session snapshot is not rewritten; this invocation only
    /// proceeds against the current configuration and records the override in
    /// telemetry. Start a new session for a permanent model change.
    #[arg(long)]
    pub allow_session_model_change: bool,

    /// Do not save the session to disk
    #[arg(long)]
    pub no_session: bool,

    /// Run in an isolated git worktree (optionally provide a name).
    ///
    /// When creating a worktree, a `.worktreeinclude` file in the project
    /// root can list glob patterns of files (e.g. `.env`, `secrets.json`) to
    /// copy into the new worktree automatically.
    #[arg(short, long, num_args = 0..=1, default_missing_value = "", value_name = "NAME")]
    pub worktree: Option<String>,

    /// Select a model by name from settings.toml
    #[arg(long)]
    pub model: Option<String>,

    /// Apply a named behavior profile from settings.toml
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,

    /// Override reasoning effort level (none, low, medium, high, xhigh, max)
    #[arg(long, value_name = "EFFORT")]
    pub reasoning_effort: Option<ReasoningEffort>,

    /// Override reasoning token budget
    #[arg(long, value_name = "TOKENS")]
    pub reasoning_budget: Option<u32>,

    /// Add a directory to the sandbox config (read-only access). Can be repeated.
    #[arg(long, value_name = "DIR")]
    pub add_dir: Vec<String>,

    /// Add a directory of user-defined toolbox tools. Can be repeated;
    /// appended after `CAKE_TOOLBOX` directories and before project-local
    /// `.cake/tools` discovery.
    #[arg(long, value_name = "DIR")]
    pub toolbox: Vec<PathBuf>,

    /// Load MCP server definitions from an `mcpServers` JSON file. Can be
    /// repeated; later files override servers with the same name.
    ///
    /// The default sources are `<config>/cake/mcp.json` and, for a trusted
    /// project, `.cake/mcp.json`. See `cake debug mcp`.
    #[arg(long, value_name = "PATH")]
    pub mcp_config: Vec<PathBuf>,

    /// Select the sandbox policy for model-generated shell commands
    /// (read-only, workspace-write, workspace-write-interactive,
    /// danger-full-access). Default: workspace-write.
    /// `workspace-write-interactive` adds the macOS capabilities that let a
    /// command launch and automate other applications. Takes precedence over
    /// `CAKE_SANDBOX`.
    #[arg(short, long, value_enum, value_name = "POLICY")]
    pub sandbox: Option<SandboxPolicy>,

    /// Only expose specific tools for this session (comma-separated list of
    /// registered names). Replaces `[tools].enabled` and profile selections.
    #[arg(long, value_name = "NAMES")]
    pub tools: Option<String>,

    /// Expose no tools for this session
    #[arg(long)]
    pub no_tools: bool,

    /// Set the provider's tool choice for this session
    ///
    /// `auto` lets the model decide, `none` forbids tool calls, and `required`
    /// forces one. The tool set is unchanged, so the request keeps its tool
    /// schemas. Ignored when the run exposes no tools.
    #[arg(long, value_enum, value_name = "MODE")]
    pub tool_choice: Option<crate::clients::ToolChoice>,

    /// Disable all skills for this session
    #[arg(long)]
    pub no_skills: bool,

    /// Only load specific skills (comma-separated list of skill names)
    #[arg(long, value_name = "NAMES")]
    pub skills: Option<String>,

    /// Path to a custom system prompt file (highest precedence)
    #[arg(long, value_name = "PATH")]
    pub system_prompt: Option<String>,

    /// Omit the process-environment Developer message from the prompt
    ///
    /// The omitted message reports the working directory, date, platform,
    /// architecture, shell, and terminal. Intended for tool-less or
    /// transcript-only subcalls; it is not a sandbox or security control.
    #[arg(long)]
    pub no_runtime_context: bool,

    /// Explicitly bypass command-policy gates for this invocation
    #[arg(long, global = true)]
    pub no_gate: bool,

    /// Trust the current project `.cake/` snapshot for this invocation only
    ///
    /// Authority-bearing project configuration (sandbox grants, gates, hooks,
    /// toolbox tools, `system_prompt`, `skills.path`) requires an explicit trust
    /// record. This flag accepts the current snapshot without writing one. The
    /// equivalent environment variable is `CAKE_TRUST_PROJECT=1`.
    #[arg(long, global = true)]
    pub trust_project: bool,

    /// Ignore project `.cake/` configuration for this invocation
    ///
    /// Settings, hooks, toolbox tools, and the project system prompt are not
    /// read. Repository content such as `AGENTS.md` remains untrusted model
    /// input. The equivalent environment variable is `CAKE_PROJECT_CONFIG=off`.
    #[arg(long, global = true)]
    pub no_project_config: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

struct PreparedRun {
    current_dir: PathBuf,
    additional_dirs: Vec<PathBuf>,
    toolbox_dirs: Vec<PathBuf>,
    mcp_config_paths: Vec<PathBuf>,
    _worktree: WorktreeGuard,
    content: String,
    output_schema: Option<Arc<crate::config::OutputSchema>>,
}

struct RunInputs {
    content: String,
    output_schema: Option<Arc<crate::config::OutputSchema>>,
}

struct RunResources {
    loaded: LoadedSettings,
    trust: ProjectTrustReport,
    project_config_mode: ProjectConfigMode,
    project_hook_files: Vec<HookFileSource>,
    agents_files: Vec<AgentsFile>,
    skill_catalog: SkillCatalog,
    tool_context: Arc<ToolContext>,
    toolbox_tools: Vec<crate::config::toolbox::ToolboxTool>,
    mcp_servers: Vec<crate::clients::mcp::McpServer>,
    tools_enabled: Option<Vec<String>>,
}

impl CodingAssistant {
    /// Maximum size in bytes for stdin input before it is rejected.
    const MAX_STDIN_SIZE: u64 = 10 * 1024 * 1024;

    /// Maximum number of `Stop`-hook follow-up turns one invocation may run.
    const MAX_STOP_FOLLOW_UPS: u32 = 1;

    /// Marker identifying the synthetic user message that carries `Stop` hook
    /// follow-up context. It is not human input.
    const STOP_FOLLOW_UP_MARKER: &str = "[Stop hook follow-up]";

    /// Read content from stdin if available (non-terminal).
    ///
    /// If stdin is a pipe or redirected file, the user explicitly connected
    /// input to cake, so read until EOF and let Ctrl-C handle stuck producers.
    ///
    /// Returns an error if stdin content exceeds [`Self::MAX_STDIN_SIZE`] to
    /// prevent unbounded memory consumption on large inputs.
    fn read_stdin_content() -> anyhow::Result<Option<String>> {
        use std::io::{IsTerminal, Read};

        if std::io::stdin().is_terminal() {
            return Ok(None);
        }

        program_status::report(
            State::Blocked(BlockedKind::Question),
            Some("Waiting for prompt input on stdin"),
        );

        let mut stdin = std::io::stdin().take(Self::MAX_STDIN_SIZE + 1);
        let mut buf = String::new();
        stdin.read_to_string(&mut buf)?;

        if buf.len() as u64 > Self::MAX_STDIN_SIZE {
            anyhow::bail!(
                "stdin input exceeds the maximum allowed size ({} MB). \
                 Pipe the content to a file first and reference the file path instead.",
                Self::MAX_STDIN_SIZE / (1024 * 1024)
            );
        }

        program_status::report(State::Working, None);

        if buf.is_empty() {
            Ok(None)
        } else {
            Ok(Some(buf))
        }
    }

    /// Build the final content from prompt and stdin according to codex-style rules.
    fn build_content(
        prompt: Option<&str>,
        stdin_content: Option<String>,
    ) -> anyhow::Result<String> {
        let stdin_content = stdin_content.filter(|s| !s.is_empty());

        match (prompt, stdin_content) {
            (Some("-"), None) => Err(anyhow::anyhow!("No input provided via stdin")),
            (Some("-") | None, Some(stdin)) => Ok(stdin),
            (Some(prompt), Some(stdin)) => {
                Ok(format!("User request:\n{prompt}\n\nStdin:\n{stdin}"))
            },
            (Some(prompt), None) => Ok(prompt.to_string()),
            (None, None) => Err(anyhow::anyhow!(
                "No input provided. Provide a prompt as an argument, use 'cake -' for stdin, or pipe input to cake."
            )),
        }
    }

    /// Resolve the configured `[[models]]` entry without CLI overrides.
    ///
    /// Provider identity is a property of the settings entry, so the snapshot
    /// and its comparison use this resolution. Per-invocation flags are applied
    /// afterward and never rewrite the stored identity.
    fn resolve_configured_model(
        &self,
        models: &HashMap<String, ModelDefinition>,
        default_model: Option<&str>,
    ) -> anyhow::Result<(ModelConfig, String)> {
        let model_name = match self.model.as_deref() {
            Some(name) => name,
            None => match default_model {
                Some(name) => name,
                None => {
                    anyhow::bail!(
                        "No model specified. cake needs a model configuration to run.\n\n\
                        Set up ~/.config/cake/settings.toml with at least one model:\n\n\
                          default_model = \"zen\"\n\n\
                          [[models]]\n\
                          name = \"zen\"\n\
                          model = \"glm-5.1\"\n\
                          base_url = \"https://opencode.ai/zen/go/v1/\"\n\
                          api_key_env = \"OPENCODE_ZEN_API_TOKEN\"\n\n\
                        Then run 'cake <prompt>' or 'cake --model zen <prompt>'."
                    );
                },
            },
        };

        if let Err(e) = ModelDefinition::validate_name(model_name) {
            anyhow::bail!(
                "Invalid model name '{model_name}': {e}. Model names must contain only lowercase letters, numbers, and hyphens."
            );
        }

        let Some(def) = models.get(model_name) else {
            let available: Vec<_> = models.keys().cloned().collect();
            let available_str = if available.is_empty() {
                String::new()
            } else {
                format!(": {}", available.join(", "))
            };
            anyhow::bail!(
                "Unknown model '{model_name}'{available_str}. Use a model name from settings.toml, or set default_model and omit --model."
            );
        };
        Ok((def.to_model_config(), model_name.to_string()))
    }

    /// Read the run choices and origin directory stored with the session being
    /// resumed.
    ///
    /// The header is read before settings load so a stored profile can shape
    /// the ordinary precedence chain instead of being applied after it. The
    /// header UUID is checked here, before extension discovery or MCP startup,
    /// so a misnamed session cannot apply another session's choices.
    fn restored_resume_config(
        run_mode: &RunMode,
        data_dir: &DataDir,
    ) -> anyhow::Result<Option<SessionResumeConfig>> {
        match run_mode {
            RunMode::Resume { session_id, .. } => data_dir.load_session_resume_config(*session_id),
            RunMode::NewSession
            | RunMode::Ephemeral
            | RunMode::ForkLatest
            | RunMode::Fork { .. } => Ok(None),
        }
    }

    /// Apply stored run choices where this invocation did not set a flag.
    ///
    /// An explicit CLI flag always wins. Repeatable directory flags replace
    /// the stored list rather than unioning, so one invocation cannot inherit a
    /// grant it did not name.
    fn with_restored_resume_config(
        &self,
        restored: Option<&SessionResumeConfig>,
        invocation_dir: &Path,
        project_config_mode: ProjectConfigMode,
    ) -> anyhow::Result<Option<Self>> {
        let Some(restored) = restored else {
            return Ok(None);
        };
        let Some(config) = restored.run_config.as_ref() else {
            return Ok(None);
        };
        let mut effective = self.clone();
        effective.apply_restored_identity(config)?;
        effective.apply_restored_paths(
            config,
            &restored.working_directory,
            invocation_dir,
            project_config_mode,
        );
        effective.apply_restored_choices(config);
        Ok(Some(effective))
    }

    fn apply_restored_identity(&mut self, config: &SessionRunConfig) -> anyhow::Result<()> {
        if self.profile.is_none() {
            self.profile.clone_from(&config.profile);
        }
        if self.sandbox.is_none()
            && let Some(label) = config.sandbox.as_deref()
        {
            self.sandbox = Some(SandboxPolicy::from_str(label, true).map_err(|error| {
                anyhow::anyhow!("Session run config has invalid sandbox policy '{label}': {error}")
            })?);
        }
        Ok(())
    }

    fn apply_restored_paths(
        &mut self,
        config: &SessionRunConfig,
        session_dir: &Path,
        invocation_dir: &Path,
        project_config_mode: ProjectConfigMode,
    ) {
        let session_dir =
            std::fs::canonicalize(session_dir).unwrap_or_else(|_| session_dir.to_path_buf());
        if self.add_dir.is_empty() {
            self.add_dir = config
                .add_dirs
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
        }

        let mut dropped = false;
        if self.toolbox.is_empty() {
            let (paths, untrusted) = restored_extension_paths(
                &config.toolbox_dirs,
                crate::config::toolbox::filter_project_toolbox_dirs,
                &session_dir,
                invocation_dir,
                project_config_mode,
            );
            self.toolbox = paths;
            dropped |= untrusted;
        }
        if self.mcp_config.is_empty() {
            let (paths, untrusted) = restored_extension_paths(
                &config.mcp_config,
                crate::config::mcp::filter_project_mcp_paths,
                &session_dir,
                invocation_dir,
                project_config_mode,
            );
            self.mcp_config = paths;
            dropped |= untrusted;
        }
        if dropped {
            eprintln!(
                "warning: ignoring resumed toolbox or MCP paths under {}: project configuration there is not trusted; run `cake trust` there or pass the flags again",
                session_dir.display()
            );
        }
    }

    fn apply_restored_choices(&mut self, config: &SessionRunConfig) {
        self.apply_restored_tool_selection(config);
        self.apply_restored_skill_selection(config);
        if self.system_prompt.is_none() {
            self.system_prompt.clone_from(&config.system_prompt);
        }
        if !self.no_runtime_context {
            self.no_runtime_context = config.no_runtime_context;
        }
    }

    fn apply_restored_tool_selection(&mut self, config: &SessionRunConfig) {
        if self.no_tools || self.tools.is_some() {
            return;
        }
        if config.no_tools {
            self.no_tools = true;
        } else {
            self.tools.clone_from(&config.tools);
        }
    }

    fn apply_restored_skill_selection(&mut self, config: &SessionRunConfig) {
        if self.no_skills || self.skills.is_some() {
            return;
        }
        if config.no_skills {
            self.no_skills = true;
        } else {
            self.skills.clone_from(&config.skills);
        }
    }

    /// Build the run choices to store with a newly created session.
    fn session_run_config(
        &self,
        prepared: &PreparedRun,
        resources: &RunResources,
        invocation_dir: &Path,
    ) -> SessionRunConfig {
        SessionRunConfig {
            profile: self.profile.clone(),
            sandbox: Some(resources.tool_context.sandbox_policy().label().to_string()),
            add_dirs: prepared.additional_dirs.clone(),
            toolbox_dirs: self
                .toolbox
                .iter()
                .map(|path| resolve_run_path(path, invocation_dir))
                .collect(),
            mcp_config: prepared.mcp_config_paths.clone(),
            tools: if self.no_tools {
                None
            } else {
                self.tools.clone()
            },
            no_tools: self.no_tools,
            skills: if self.no_skills {
                None
            } else {
                self.skills.clone()
            },
            no_skills: self.no_skills,
            system_prompt: self.system_prompt.as_deref().map(|path| {
                resolve_system_prompt_path(path, &prepared.current_dir)
                    .to_string_lossy()
                    .into_owned()
            }),
            no_runtime_context: self.no_runtime_context,
        }
    }

    /// Resolve the model for a session restore (--resume or --fork).
    ///
    /// Returns the configured resolution before CLI overrides and before any
    /// credential is read, so the caller can compare the stored provider
    /// identity first. Sessions persist both the provider model ID and the
    /// entry name; the name is the preferred identity because the ID alone is
    /// ambiguous when several entries share one ID.
    ///
    /// Policy:
    /// - Explicit `--model` naming a different stored entry errors, even when
    ///   both entries share one provider model ID.
    /// - Without `--model`, a stored entry name resolves deterministically.
    /// - Legacy sessions with only a model ID resolve it when it matches
    ///   exactly one entry and error with candidates when it is ambiguous.
    /// - With neither, fall back to default model resolution.
    fn resolve_model_selection(
        &self,
        models: &HashMap<String, ModelDefinition>,
        default_model: Option<&str>,
        session_model: Option<&str>,
        session_config: Option<&str>,
    ) -> anyhow::Result<(ModelConfig, String)> {
        if self.model.is_some() {
            self.resolve_explicit_session_model(
                models,
                default_model,
                session_model,
                session_config,
            )
        } else if let Some(stored_config) = session_config {
            Self::resolve_stored_config_model(models, stored_config)
        } else if let Some(model_id) = session_model {
            Self::resolve_legacy_session_model(models, model_id)
        } else {
            self.resolve_configured_model(models, default_model)
        }
    }

    /// Resolve a restore when `--model` names an entry explicitly.
    ///
    /// A stored entry name that differs from the requested one errors, even
    /// when both entries share one provider model ID: silently switching
    /// would flip the backend or reasoning effort mid-session.
    fn resolve_explicit_session_model(
        &self,
        models: &HashMap<String, ModelDefinition>,
        default_model: Option<&str>,
        session_model: Option<&str>,
        session_config: Option<&str>,
    ) -> anyhow::Result<(ModelConfig, String)> {
        let (config, config_name) = self.resolve_configured_model(models, default_model)?;
        if let Some(stored_config) = session_config
            && stored_config != config_name
        {
            anyhow::bail!(
                "Session model mismatch: session uses config '{stored_config}' but --model '{config_name}' was given. \
                 Use --model {stored_config} to continue with the session's model, or start a new session."
            );
        }
        if session_config.is_none()
            && let Some(stored_id) = session_model
            && stored_id != config.model
        {
            anyhow::bail!(
                "Session model mismatch: session uses '{stored_id}' but --model resolves to '{}'. \
                 Use --model {stored_id} to continue with the session's model, or start a new session.",
                config.model
            );
        }
        Ok((config, config_name))
    }

    /// Resolve a restore when the session names its `[[models]]` entry.
    ///
    /// Deterministic by construction: the name indexes one entry.
    fn resolve_stored_config_model(
        models: &HashMap<String, ModelDefinition>,
        stored_config: &str,
    ) -> anyhow::Result<(ModelConfig, String)> {
        let Some(def) = models.get(stored_config) else {
            anyhow::bail!(
                "Session model config '{stored_config}' is not configured in settings.toml. \
                 Add a [[models]] entry with name = \"{stored_config}\" to continue this session, \
                 or start a new session."
            );
        };
        Ok((def.to_model_config(), def.name.clone()))
    }

    /// Resolve a legacy restore carrying only a provider model ID.
    ///
    /// A unique ID resolves to its entry; a shared ID errors with the
    /// sorted candidate entry names instead of picking nondeterministically.
    fn resolve_legacy_session_model(
        models: &HashMap<String, ModelDefinition>,
        model_id: &str,
    ) -> anyhow::Result<(ModelConfig, String)> {
        let mut candidates: Vec<&ModelDefinition> = models
            .values()
            .filter(|def| def.model == model_id)
            .collect();
        candidates.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        match candidates.as_slice() {
            [] => {
                anyhow::bail!(
                    "Session model '{model_id}' is not configured in settings.toml. \
                     Add a [[models]] entry for '{model_id}' to continue this session, \
                     or start a new session."
                );
            },
            [only] => Ok((only.to_model_config(), only.name.clone())),
            _ => {
                let candidate_names = candidates
                    .iter()
                    .map(|def| def.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::bail!(
                    "Session model '{model_id}' is ambiguous: matches {} model configs [{}]. \
                     Re-run with explicit --model <name> to continue this session, \
                     or start a new session.",
                    candidates.len(),
                    candidate_names
                );
            },
        }
    }

    /// Apply CLI overrides (`max_tokens`, `reasoning_effort`, `reasoning_budget`) to a
    /// configured model config.
    const fn apply_cli_overrides_to_config(&self, mut config: ModelConfig) -> ModelConfig {
        if let Some(max_tokens) = self.max_tokens {
            config.max_output_tokens = Some(max_tokens);
        }
        if let Some(effort) = self.reasoning_effort {
            config.reasoning_effort = Some(effort);
        }
        if let Some(budget) = self.reasoning_budget {
            config.reasoning_max_tokens = Some(budget);
        }
        config
    }

    /// Set up a worktree if `--worktree` was provided.
    fn setup_worktree(
        &self,
        original_dir: &std::path::Path,
    ) -> anyhow::Result<Option<worktree::Worktree>> {
        let Some(ref wt_name) = self.worktree else {
            return Ok(None);
        };

        let name = if wt_name.is_empty() {
            None
        } else {
            Some(wt_name.as_str())
        };

        let wt = worktree::create(original_dir, name)?;
        eprintln!("Working in worktree '{}' ({})", wt.name, wt.path.display());
        std::env::set_current_dir(&wt.path)
            .map_err(|e| anyhow::anyhow!("Failed to cd into worktree: {e}"))?;
        Ok(Some(wt))
    }

    /// Resolve `--add-dir` values against the startup directory.
    ///
    /// Returns absolute, canonical paths that remain valid even when the
    /// process cwd changes later (e.g., via `--worktree`).
    fn resolve_additional_dirs(&self, base_dir: &Path) -> Vec<PathBuf> {
        self.add_dir
            .iter()
            .filter_map(|dir| {
                let path = PathBuf::from(dir);
                let path_to_check = if path.is_absolute() {
                    path
                } else {
                    base_dir.join(&path)
                };
                if path_to_check.exists() && path_to_check.is_dir() {
                    Some(std::fs::canonicalize(&path_to_check).unwrap_or(path_to_check))
                } else {
                    tracing::warn!(
                        "--add-dir path '{dir}' does not exist or is not a directory, ignoring"
                    );
                    None
                }
            })
            .collect()
    }

    fn prepare_run(&self, project_config_mode: ProjectConfigMode) -> anyhow::Result<PreparedRun> {
        let original_dir = std::env::current_dir()?;
        let additional_dirs = self.resolve_additional_dirs(&original_dir);
        let inputs = self.validate_run_inputs(&original_dir)?;

        let worktree = self.setup_worktree(&original_dir)?;
        let current_dir = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("Failed to get current directory: {e}"))?;
        let mut toolbox_dirs = crate::config::toolbox::toolbox_directories(
            std::env::var(crate::config::toolbox::TOOLBOX_ENV_VAR)
                .ok()
                .as_deref(),
            &self.toolbox,
            &crate::config::config_dir().join("cake").join("tools"),
            &original_dir,
            &current_dir,
        );
        if project_config_mode == ProjectConfigMode::Ignore {
            toolbox_dirs = crate::config::toolbox::filter_project_toolbox_dirs(
                toolbox_dirs,
                &[&original_dir, &current_dir],
            );
        }

        let mut mcp_config_paths: Vec<PathBuf> = self
            .mcp_config
            .iter()
            .map(|path| {
                if path.is_absolute() {
                    path.clone()
                } else {
                    original_dir.join(path)
                }
            })
            .collect();
        if project_config_mode == ProjectConfigMode::Ignore {
            mcp_config_paths = crate::config::mcp::filter_project_mcp_paths(
                mcp_config_paths,
                &[&original_dir, &current_dir],
            );
        }

        Ok(PreparedRun {
            current_dir,
            additional_dirs,
            toolbox_dirs,
            mcp_config_paths,
            _worktree: WorktreeGuard::new(worktree, original_dir),
            content: inputs.content,
            output_schema: inputs.output_schema,
        })
    }

    fn validate_run_inputs(&self, original_dir: &Path) -> anyhow::Result<RunInputs> {
        let stdin_content = Self::read_stdin_content()?;
        let content = Self::build_content(self.prompt.as_deref(), stdin_content)?;
        let output_schema = self.load_output_schema(original_dir)?;
        Ok(RunInputs {
            content,
            output_schema,
        })
    }

    /// Resolve the project-configuration mode for this invocation.
    fn resolve_project_config_mode(
        &self,
    ) -> Result<ProjectConfigMode, crate::config::trust::ProjectConfigModeError> {
        let surface = self
            .command
            .as_ref()
            .map_or(ProjectConfigUse::Enforce, Commands::project_config_use);
        ProjectConfigMode::resolve(surface, self.trust_project, self.no_project_config)
    }

    fn load_settings(
        &self,
        current_dir: &Path,
        mode: ProjectConfigMode,
    ) -> anyhow::Result<crate::config::settings::SettingsLoad> {
        Ok(SettingsLoader::load_with_project_config(
            current_dir,
            self.profile.as_deref(),
            self.no_gate,
            &crate::config::config_dir(),
            mode,
        )?)
    }

    /// Prepare the invocation directory and load every run resource.
    async fn prepare_run_context(
        &self,
        data_dir: &DataDir,
        project_config_mode: ProjectConfigMode,
    ) -> anyhow::Result<(PreparedRun, RunResources)> {
        let prepared = self.prepare_run(project_config_mode)?;
        let resources = self
            .load_run_resources(
                &prepared.current_dir,
                data_dir,
                prepared.additional_dirs.clone(),
                &prepared.toolbox_dirs,
                &prepared.mcp_config_paths,
                project_config_mode,
            )
            .await?;
        Ok((prepared, resources))
    }

    async fn load_run_resources(
        &self,
        current_dir: &Path,
        data_dir: &DataDir,
        additional_dirs: Vec<PathBuf>,
        toolbox_dirs: &[PathBuf],
        mcp_config_paths: &[PathBuf],
        project_config_mode: ProjectConfigMode,
    ) -> anyhow::Result<RunResources> {
        let load = self.load_settings(current_dir, project_config_mode)?;
        let loaded = load.settings;
        let trust = load.trust;
        let project_hook_files = load.hook_files;
        let project_mcp_bytes = load.mcp_config_bytes;
        loaded.print_warnings();
        let agents_files = read_agents_files(current_dir);

        let skill_config = SettingsLoader::resolve_skill_config(
            self.no_skills,
            self.skills.as_deref(),
            &loaded.skills,
        );

        let tools_enabled = SettingsLoader::resolve_tools_config(
            self.no_tools,
            self.tools.as_deref(),
            loaded.tools_enabled.as_deref(),
        );

        let configured_skill_dirs = loaded
            .skills
            .path
            .as_deref()
            .map(parse_skill_path_list)
            .unwrap_or_default();
        let mut skill_catalog = if configured_skill_dirs.is_empty() {
            discover_skills(current_dir)
        } else {
            discover_skills_with_paths(current_dir, &configured_skill_dirs)
        };
        skill_catalog = skill_config.apply(skill_catalog);

        let skill_base_dirs: Vec<PathBuf> = skill_catalog
            .skills
            .iter()
            .map(|s| s.base_directory.clone())
            .collect();

        let sandbox_policy = resolve_sandbox_policy(self.sandbox);
        let settings_dirs = Self::writable_dirs(&loaded, sandbox_policy)?;
        let additional_dirs = Self::read_only_dirs(
            additional_dirs,
            Self::valid_sandbox_read_only_dirs(&loaded),
            data_dir,
            &crate::config::config_dir().join("cake"),
        );
        let tool_context = ToolContext::new(
            current_dir.to_path_buf(),
            &additional_dirs,
            &skill_base_dirs,
            &settings_dirs,
            sandbox_policy,
        )
        .with_limits(loaded.limits.tool_limits);

        let linked_dirs = resolve_linked_worktree_dirs(current_dir);
        if linked_dirs.is_empty() && current_dir.join(".git").is_file() {
            eprintln!(
                "warning: workspace is a linked git worktree but the worktree gitdir \
                 could not be resolved. Git commands (status, add, commit) may fail \
                 under the sandbox. See the cake log file for details."
            );
        }

        Self::log_skill_diagnostics(&skill_catalog);

        let toolbox_tools = if sandbox_policy == SandboxPolicy::ReadOnly {
            Vec::new()
        } else {
            crate::config::toolbox::load_toolbox_tools(toolbox_dirs).await
        };

        let mcp_servers = if sandbox_policy == SandboxPolicy::ReadOnly {
            if !mcp_config_paths.is_empty() {
                eprintln!("note: MCP servers are not started under the read-only sandbox policy");
            }
            Vec::new()
        } else {
            Self::start_mcp_servers(
                current_dir,
                mcp_config_paths,
                &trust,
                project_config_mode,
                project_mcp_bytes.as_deref(),
            )
            .await?
        };

        Ok(RunResources {
            loaded,
            trust,
            project_config_mode,
            project_hook_files,
            agents_files,
            skill_catalog,
            tool_context: Arc::new(tool_context),
            toolbox_tools,
            mcp_servers,
            tools_enabled,
        })
    }

    /// Discover configured MCP servers, connect to each, and print warnings for
    /// servers that cannot start. A broken server is skipped; it never stops the
    /// run.
    async fn start_mcp_servers(
        current_dir: &Path,
        mcp_config_paths: &[PathBuf],
        trust: &ProjectTrustReport,
        project_config_mode: ProjectConfigMode,
        project_mcp_bytes: Option<&[u8]>,
    ) -> anyhow::Result<Vec<crate::clients::mcp::McpServer>> {
        let discovery = crate::config::mcp::discover_mcp_servers(
            mcp_config_paths,
            current_dir,
            &crate::config::config_dir(),
            current_dir,
            trust,
            project_config_mode,
            project_mcp_bytes,
        )?;
        for warning in &discovery.warnings {
            eprintln!("warning: {warning}");
        }
        let mut servers = Vec::new();
        for config in &discovery.servers {
            if config.exposure == crate::config::mcp::McpExposure::Hidden {
                continue;
            }
            match crate::clients::mcp::connect_server(config, current_dir).await {
                Ok(server) => servers.push(server),
                Err(error) => {
                    eprintln!("warning: skipping MCP server '{}': {error}", config.name);
                },
            }
        }
        Ok(servers)
    }

    /// Resolve `[sandbox].read_only` paths for read-only sandbox access.
    ///
    /// Unlike writable grants, entries may be individual executable files
    /// (e.g. `~/.local/bin/claude`) as well as directories. Nonexistent paths
    /// are ignored with a file-only log warning.
    fn valid_sandbox_read_only_dirs(loaded: &LoadedSettings) -> Vec<PathBuf> {
        loaded
            .sandbox
            .read_only
            .iter()
            .map(PathBuf::from)
            .filter(|p| {
                if p.exists() {
                    true
                } else {
                    tracing::warn!(
                        "settings.toml [sandbox].read_only path '{}' does not exist, ignoring",
                        p.display()
                    );
                    false
                }
            })
            .collect()
    }

    /// Build the writable path grant list for a run.
    ///
    /// Configured `directories` and `[sandbox].writable` targets are provisioned
    /// first. The user skill root is a built-in writable default for writable
    /// policies and a read-only grant under `read-only`; it is granted even
    /// when `--no-skills` disables discovery.
    fn writable_dirs(
        loaded: &LoadedSettings,
        sandbox_policy: SandboxPolicy,
    ) -> anyhow::Result<Vec<PathBuf>> {
        let mut dirs = crate::cli::prepare_writable_dirs(
            &loaded.directories,
            &loaded.sandbox.writable,
            sandbox_policy,
        )?;
        if let Some(user_skills) = user_skills_dir()
            && let Some(dir) = prepare_user_skills_dir(&user_skills, sandbox_policy)
        {
            dirs.push(dir);
        }
        Ok(dirs)
    }

    /// Build the read-only path grant list for a run.
    ///
    /// `--add-dir` entries and `[sandbox].read_only` settings grants join
    /// Cake's own config directory and state roots (session files, telemetry,
    /// and logs) so the configuration and debugging skills can read their
    /// inputs without a manual grant. The list feeds both the OS sandbox and
    /// the in-process Read/ReadImage/Edit/Write path checks. See
    /// [`DataDir::sandbox_read_dirs`] as well.
    fn read_only_dirs(
        additional_dirs: Vec<PathBuf>,
        settings_read_only: Vec<PathBuf>,
        data_dir: &DataDir,
        config_dir: &Path,
    ) -> Vec<PathBuf> {
        let mut dirs = additional_dirs;
        dirs.extend(settings_read_only);
        dirs.extend(data_dir.sandbox_read_dirs());
        dirs.push(config_dir.to_path_buf());
        dirs
    }

    fn log_skill_diagnostics(skill_catalog: &SkillCatalog) {
        for diagnostic in &skill_catalog.diagnostics {
            match diagnostic.level {
                DiagnosticLevel::Warning => {
                    if diagnostic.file.as_os_str().is_empty() {
                        tracing::warn!("{}", diagnostic.message);
                    } else {
                        tracing::warn!(
                            "Skill diagnostic ({}): {}",
                            diagnostic.file.display(),
                            diagnostic.message
                        );
                    }
                },
                DiagnosticLevel::Error => {
                    if diagnostic.file.as_os_str().is_empty() {
                        tracing::error!("{}", diagnostic.message);
                    } else {
                        tracing::error!(
                            "Skill diagnostic ({}): {}",
                            diagnostic.file.display(),
                            diagnostic.message
                        );
                    }
                },
            }
        }

        if !skill_catalog.skills.is_empty() {
            tracing::info!(
                "Discovered {} skill(s): {}",
                skill_catalog.skills.len(),
                skill_catalog
                    .skills
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    fn unavailable_tool_names(requested: Option<&[String]>, available: &[String]) -> Vec<String> {
        let Some(requested) = requested else {
            return Vec::new();
        };
        let mut seen = HashSet::with_capacity(requested.len());
        requested
            .iter()
            .filter(|name| {
                !available
                    .iter()
                    .any(|available_name| available_name == *name)
            })
            .filter(|name| seen.insert(name.as_str()))
            .cloned()
            .collect()
    }

    fn warn_unavailable_tools(requested: Option<&[String]>, available: &[String]) {
        for name in Self::unavailable_tool_names(requested, available) {
            eprintln!("warning: configured tool '{name}' is not available in this session");
        }
    }

    /// Apply the automatic compaction policy to an agent.
    ///
    /// A disabled policy resolves no summarizer credentials, so a configured
    /// summarizer model stays inert until compaction is enabled. An enabled
    /// policy resolves the separate summarizer configuration once and validates
    /// that backend before the run starts.
    fn apply_compaction_policy(
        agent: Agent,
        loaded: &LoadedSettings,
        conversation: &ResolvedModelConfig,
    ) -> anyhow::Result<Agent> {
        if !loaded.compaction.enabled {
            return agent.with_compaction(loaded.compaction, None);
        }
        let summarizer = loaded.resolve_compaction_summarizer(conversation)?;
        agent.with_compaction(loaded.compaction, summarizer)
    }

    fn attach_persistence(
        mut client: Agent,
        data_dir: &DataDir,
        session: &Session,
        plan: Option<SessionPersistencePlan>,
    ) -> anyhow::Result<(Agent, Option<crate::config::SessionWriter>)> {
        let Some(file) = execute_persistence_plan(plan, data_dir, session, client.tool_names())?
        else {
            return Ok((client, None));
        };

        let writer = crate::config::SessionWriter::new(
            file,
            Arc::new(data_dir.session_blob_store(session.id)),
        );
        let writer_for_callback = writer.clone();
        let writer_for_health = writer.clone();
        client = client
            .with_persist_callback(move |record| writer_for_callback.append_record(record))
            .with_health_callback(move || writer_for_health.ensure_healthy());
        Ok((client, Some(writer)))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "telemetry setup threads session identity, mode, and override state through one call"
    )]
    fn attach_session_telemetry(
        client: Agent,
        data_dir: &DataDir,
        session: &Session,
        run_mode: &RunMode,
        output_format: OutputFormat,
        project_trust: &ProjectTrustReport,
        effective_provider_identity: Option<&crate::types::ProviderIdentity>,
        session_model_change_override: bool,
    ) -> Agent {
        if !run_mode.persists_session() {
            return client;
        }

        let path = data_dir.session_telemetry_path(session.id);
        let mut writer = match SessionTelemetryWriter::open(&path) {
            Ok(writer) => writer,
            Err(error) => {
                tracing::warn!(
                    target: "cake",
                    "Disabling session telemetry; failed to open {}: {error}",
                    path.display()
                );
                return client;
            },
        };

        let invocation_id = uuid::Uuid::new_v4();
        let settings =
            client.session_telemetry_settings(output_format, session.model_config.clone());
        let project_trust = crate::session_telemetry::ProjectTrustTelemetry {
            mode: project_trust.telemetry_mode(),
            project_key: (!matches!(
                project_trust.state,
                crate::config::trust::TrustState::NotRequired
            ))
            .then(|| project_trust.project_key.clone()),
            authority_files: project_trust.authority_file_count(),
        };
        let record = SessionTelemetryRecord::TelemetryInit {
            session_id: session.id.to_string(),
            invocation_id: invocation_id.to_string(),
            timestamp: chrono::Utc::now(),
            mode: run_mode.telemetry_mode(),
            model: client.model_name().to_string(),
            api_type: settings.api_type,
            output_format,
            tools: client.tool_names(),
            settings,
            build_commit: crate::build_info::commit(),
            build_dirty: crate::build_info::dirty(),
            project_trust: Some(project_trust),
            provider_identity: effective_provider_identity
                .map(crate::session_telemetry::ProviderIdentityTelemetry::from),
            session_model_change_override,
        };

        if let Err(error) = writer.append(&record) {
            tracing::warn!(
                target: "cake",
                "Disabling session telemetry; failed to write init record to {}: {error}",
                path.display()
            );
            return client;
        }

        client.with_session_telemetry(writer, invocation_id)
    }

    fn attach_hooks(
        mut client: Agent,
        hook_context: HookContext,
        hook_output_limit: Option<usize>,
        project_hook_sources: &[HookFileSource],
    ) -> anyhow::Result<(Agent, Option<Arc<HookRunner>>)> {
        let global_hooks = crate::config::config_dir().join("cake").join("hooks.json");
        let hooks =
            HooksLoader::load_with_project_sources(Some(&global_hooks), project_hook_sources)?;

        if hooks.is_empty() {
            return Ok((client, None));
        }

        let runner =
            Arc::new(HookRunner::new(hooks, hook_context).with_output_limit(hook_output_limit));
        client = client.with_hook_runner(Arc::clone(&runner));
        Ok((client, Some(runner)))
    }

    /// Build the sole owner of hook-event emission.
    ///
    /// The returned closure fans each hook record out to session persistence
    /// (when a writer exists) and, for stream-json runs, to stdout. Because it
    /// is the only emission path, `HookContext` carries no separate writer and
    /// there is no sink-versus-writer precedence to reason about.
    fn hook_event_sink(
        session_writer: Option<crate::config::SessionWriter>,
        output_format: OutputFormat,
    ) -> Option<Arc<dyn Fn(StreamRecord) -> anyhow::Result<()> + Send + Sync>> {
        if session_writer.is_none() && output_format != OutputFormat::StreamJson {
            return None;
        }

        Some(Arc::new(move |record| {
            if let Some(writer) = &session_writer {
                let session_record = SessionRecord::from(record.clone());
                if let Err(error) = writer.append_record(&session_record) {
                    tracing::warn!(
                        target: "cake::hooks",
                        error = %error,
                        "Failed to append hook transcript record"
                    );
                }
            }

            if output_format == OutputFormat::StreamJson {
                match serde_json::to_string(&record) {
                    Ok(json) => CliOutputSink::write_stream_record(&json)?,
                    Err(error) => tracing::warn!("Stream serialization failed: {error}"),
                }
            }
            Ok(())
        }))
    }

    /// Reject options that belong only to the root agent run when a
    /// subcommand is selected. Model, profile, and output format are omitted
    /// because command runners receive those through `CommandRunOptions`.
    fn validate_subcommand_options(&self) -> anyhow::Result<()> {
        let Some(command) = self.command.as_ref() else {
            return Ok(());
        };
        let rejects_project_trust = command.rejects_project_trust_flags();

        let invalid = [
            (self.prompt.is_some(), "prompt"),
            (self.fail_on_error, "--fail-on-error"),
            (self.max_tokens.is_some(), "--max-tokens"),
            (self.output_schema.is_some(), "--output-schema"),
            (self.resume.is_some(), "--resume"),
            (self.read_only, "--read-only"),
            (self.fork.is_some(), "--fork"),
            (
                self.allow_session_model_change,
                "--allow-session-model-change",
            ),
            (self.no_session, "--no-session"),
            (self.worktree.is_some(), "--worktree"),
            (self.reasoning_effort.is_some(), "--reasoning-effort"),
            (self.reasoning_budget.is_some(), "--reasoning-budget"),
            (!self.add_dir.is_empty(), "--add-dir"),
            (!self.toolbox.is_empty(), "--toolbox"),
            (!self.mcp_config.is_empty(), "--mcp-config"),
            (self.sandbox.is_some(), "--sandbox"),
            (self.no_tools, "--no-tools"),
            (self.tool_choice.is_some(), "--tool-choice"),
            (self.tools.is_some(), "--tools"),
            (self.no_skills, "--no-skills"),
            (self.skills.is_some(), "--skills"),
            (self.system_prompt.is_some(), "--system-prompt"),
            (self.no_runtime_context, "--no-runtime-context"),
            (
                self.trust_project && rejects_project_trust,
                "--trust-project",
            ),
            (
                self.no_project_config && rejects_project_trust,
                "--no-project-config",
            ),
        ]
        .into_iter()
        .filter_map(|(present, option)| present.then_some(option))
        .collect::<Vec<_>>();

        if invalid.is_empty() {
            return Ok(());
        }

        Err(crate::cli::InvalidSubcommandOptions::new(command.name(), &invalid).into())
    }

    const fn output_sink(&self) -> CliOutputSink {
        CliOutputSink::new(self.output_format, self.fail_on_error)
    }

    /// Load and compile the `--output-schema` file, if given.
    ///
    /// Runs before any run state is created (worktrees, sessions, task
    /// records) so bad input fails fast with exit code 3 and no `task_start`
    /// is emitted. The path resolves against the startup directory.
    fn load_output_schema(
        &self,
        base_dir: &Path,
    ) -> anyhow::Result<Option<Arc<crate::config::OutputSchema>>> {
        match self.output_schema.as_deref() {
            Some(path) => {
                let resolved = base_dir.join(path);
                Ok(Some(Arc::new(crate::config::OutputSchema::load(
                    &resolved,
                )?)))
            },
            None => Ok(None),
        }
    }

    async fn execute_agent_turn(
        client: &mut Agent,
        hook_runner: Option<&Arc<HookRunner>>,
        output: CliOutputSink,
        session_start_source: HookSource,
        content: &str,
        follow_ups_allowed: bool,
    ) -> anyhow::Result<TurnResult> {
        let start = Instant::now();

        Self::prepare_agent_turn(client, hook_runner, &session_start_source, content).await?;

        let max_follow_ups = if follow_ups_allowed {
            Self::MAX_STOP_FOLLOW_UPS
        } else {
            0
        };
        let mut prompt = content.to_string();
        let mut intermediate = Vec::new();
        let mut follow_ups_run = 0_u32;

        let result = loop {
            let result = client.send(prompt).await;
            let Ok(text) = &result else {
                break result;
            };

            let outcome =
                Self::run_stop_hook(hook_runner, text, follow_ups_run, max_follow_ups).await;
            let Some(follow_up_context) =
                Self::stop_follow_up_context(&outcome, follow_ups_run, max_follow_ups)
            else {
                break result;
            };

            if let Err(error) = output.render_intermediate(text, Self::STOP_FOLLOW_UP_MARKER) {
                if CliOutputSink::is_closed_output(&error) {
                    break result;
                }
                return Err(error);
            }
            intermediate.push(text.clone());
            follow_ups_run += 1;
            tracing::info!(
                target: "cake::hooks",
                follow_up_index = follow_ups_run,
                "Stop hook requested a follow-up turn"
            );
            prompt = Self::stop_follow_up_prompt(&follow_up_context);
        };

        let duration_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

        Self::handle_agent_turn_result(client, hook_runner, &result, duration_ms).await?;

        Ok(TurnResult {
            result,
            duration_ms,
            intermediate,
        })
    }

    /// The context for one follow-up turn, or `None` when the hook did not ask,
    /// the budget is spent, or there is nothing to deliver.
    fn stop_follow_up_context(
        outcome: &StopHookOutcome,
        follow_ups_run: u32,
        max_follow_ups: u32,
    ) -> Option<String> {
        if !outcome.follow_up || follow_ups_run >= max_follow_ups {
            return None;
        }
        outcome
            .context
            .clone()
            .filter(|context| !context.is_empty())
    }

    /// The synthetic user message that carries follow-up context. The marker
    /// keeps the turn out of the human's voice in the transcript and on the
    /// wire.
    fn stop_follow_up_prompt(context: &str) -> String {
        format!("{}\n{context}", Self::STOP_FOLLOW_UP_MARKER)
    }

    /// Run everything that precedes `client.send()`: the session-start and
    /// prompt-submit hooks, the output-schema and prompt-context records, the
    /// task-start record, and history repair.
    ///
    /// A setup failure dispatches a best-effort `SessionEnd` with `Error`
    /// before returning the original error, so a reporter that saw `working`
    /// still gets a cleanup boundary. Extracted from `execute_agent_turn` so
    /// the turn itself stays under the complexity ceiling.
    async fn prepare_agent_turn(
        client: &mut Agent,
        hook_runner: Option<&Arc<HookRunner>>,
        session_start_source: &HookSource,
        content: &str,
    ) -> anyhow::Result<()> {
        let setup: anyhow::Result<()> = async {
            if let Some(runner) = hook_runner {
                let contexts = runner.session_start(session_start_source, content).await?;
                client.append_developer_context(contexts);
                let contexts = runner.user_prompt_submit(content).await?;
                client.append_developer_context(contexts);
            }
            client.append_output_schema_context();
            client.emit_prompt_context_records()?;
            client.emit_task_start_record()?;
            client.emit_history_repair_records()?;
            Ok(())
        }
        .await;

        if let Err(error) = setup {
            Self::run_session_end_hook(hook_runner, SessionEndReason::Error).await;
            return Err(error);
        }
        Ok(())
    }

    /// Handle the result of the last `client.send()`: invoke error hooks and
    /// emit the task completion record. `Stop` already ran for the turn in
    /// `execute_agent_turn`, so success only closes the invocation. Extracted
    /// for focused unit-testing without requiring a real API call.
    async fn handle_agent_turn_result(
        client: &mut Agent,
        hook_runner: Option<&Arc<HookRunner>>,
        result: &Result<String, anyhow::Error>,
        duration_ms: u64,
    ) -> anyhow::Result<()> {
        match result {
            Ok(response_text) => {
                Self::emit_successful_agent_turn(client, hook_runner, response_text, duration_ms)
                    .await?;
            },
            Err(error) => {
                if let Some(cutoff) = error.downcast_ref::<CutOffError>() {
                    Self::emit_cut_off_agent_turn(client, hook_runner, cutoff, duration_ms).await?;
                } else {
                    Self::emit_failed_agent_turn(client, hook_runner, error, duration_ms).await?;
                }
            },
        }
        Ok(())
    }

    /// Close a successful invocation after `Stop` has already run for its
    /// final turn.
    async fn emit_successful_agent_turn(
        client: &mut Agent,
        hook_runner: Option<&Arc<HookRunner>>,
        response_text: &str,
        duration_ms: u64,
    ) -> anyhow::Result<()> {
        Self::run_session_end_hook(hook_runner, SessionEndReason::Success).await;
        client.emit_task_complete_record(
            TaskOutcome::Success {
                result: Some(response_text.to_string()),
            },
            duration_ms,
        )
    }

    async fn emit_failed_agent_turn(
        client: &mut Agent,
        hook_runner: Option<&Arc<HookRunner>>,
        error: &anyhow::Error,
        duration_ms: u64,
    ) -> anyhow::Result<()> {
        Self::run_error_hook(hook_runner, error).await;
        Self::run_session_end_hook(hook_runner, SessionEndReason::Error).await;
        Self::append_task_completion(client, Self::task_outcome_for_error(error), duration_ms);
        Ok(())
    }

    async fn emit_cut_off_agent_turn(
        client: &mut Agent,
        hook_runner: Option<&Arc<HookRunner>>,
        cutoff: &CutOffError,
        duration_ms: u64,
    ) -> anyhow::Result<()> {
        Self::run_stop_hook(hook_runner, &cutoff.detail, 0, 0).await;
        Self::run_session_end_hook(hook_runner, SessionEndReason::Error).await;
        Self::append_task_completion(
            client,
            TaskOutcome::CutOff {
                detail: cutoff.detail.clone(),
            },
            duration_ms,
        );
        Ok(())
    }

    /// Append a completion record without masking the failure it describes.
    fn append_task_completion(client: &mut Agent, outcome: TaskOutcome, duration_ms: u64) {
        if let Err(emit_error) = client.emit_task_complete_record(outcome, duration_ms) {
            tracing::warn!(
                target: "cake",
                error = %emit_error,
                "Failed to append task completion record"
            );
        }
    }

    async fn run_stop_hook(
        hook_runner: Option<&Arc<HookRunner>>,
        response_text: &str,
        follow_up_index: u32,
        max_follow_ups: u32,
    ) -> StopHookOutcome {
        let Some(runner) = hook_runner else {
            return StopHookOutcome::default();
        };
        match runner
            .stop(response_text, follow_up_index, max_follow_ups)
            .await
        {
            Ok(outcome) => {
                if let Some(context) = outcome.context.as_ref() {
                    tracing::info!(
                        target: "cake::hooks",
                        additional_context = %context,
                        follow_up = outcome.follow_up,
                        "Stop hook returned additional context"
                    );
                }
                outcome
            },
            Err(error) => {
                tracing::warn!(target: "cake::hooks", error = %error, "Stop hook failed (best-effort)");
                StopHookOutcome::default()
            },
        }
    }

    async fn run_error_hook(hook_runner: Option<&Arc<HookRunner>>, error: &anyhow::Error) {
        let Some(runner) = hook_runner else {
            return;
        };
        if let Err(hook_error) = runner.error_occurred(&sink_safe_error_text(error)).await {
            tracing::warn!(target: "cake::hooks", error = %hook_error, "error_occurred hook failed (best-effort)");
        }
    }

    async fn run_session_end_hook(hook_runner: Option<&Arc<HookRunner>>, reason: SessionEndReason) {
        let Some(runner) = hook_runner else {
            return;
        };
        if let Err(error) = runner.session_end(reason).await {
            tracing::warn!(
                target: "cake::hooks",
                reason = reason.as_str(),
                error = %error,
                "SessionEnd hook failed (best-effort)"
            );
        }
    }

    fn task_outcome_for_error(error: &anyhow::Error) -> TaskOutcome {
        if let Some(cutoff) = error.downcast_ref::<CutOffError>() {
            return TaskOutcome::CutOff {
                detail: cutoff.detail.clone(),
            };
        }
        if let Some(limit_exceeded) = error.downcast_ref::<LimitExceededError>() {
            return TaskOutcome::LimitExceeded {
                limit: limit_exceeded.limit.clone(),
                detail: limit_exceeded.detail.clone(),
                result: limit_exceeded.result.clone(),
            };
        }
        if is_output_schema_exhaustion(error) {
            TaskOutcome::ErrorOutputSchema {
                error: format!("{error:#}"),
            }
        } else {
            TaskOutcome::ErrorDuringExecution {
                error: format!("{error:#}"),
            }
        }
    }

    /// Report the turn's result as the run's final terminal state.
    ///
    /// The turn result, not the process exit code, is the authority: JSON runs
    /// and stream-json runs without `--fail-on-error` report a failed turn in
    /// their own output and still exit `0`, and a terminal showing `done` for
    /// them would be wrong. `msg` carries the same sink-safe error text the run
    /// prints on stderr.
    fn report_turn_outcome(result: &Result<String, anyhow::Error>) {
        match result {
            Ok(_) => program_status::report(State::Done, None),
            Err(error) => {
                program_status::report(State::Error, Some(&sink_safe_error_text(error)));
            },
        }
    }
}

fn is_output_schema_exhaustion(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<crate::config::OutputSchemaError>(),
        Some(crate::config::OutputSchemaError::Unsatisfied { .. })
    )
}

/// Error text safe for out-of-band sinks (telemetry summary, error hooks).
///
/// A [`LimitExceededError`]'s `Display` embeds the partial assistant message
/// so text mode can surface it, but these sinks must not carry assistant
/// text; they get the concise detail instead. Typed provider errors keep a
/// bounded response preview for human diagnostics but are reduced to their
/// preview-free detail. Other errors retain their full cause chain.
fn sink_safe_error_text(error: &anyhow::Error) -> String {
    if let Some(limit_exceeded) = error.downcast_ref::<LimitExceededError>() {
        return limit_exceeded.detail.clone();
    }
    if let Some(detail) = crate::clients::telemetry_safe_provider_error_detail(error) {
        return detail;
    }
    format!("{error:#}")
}

fn resolve_run_path(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// Resolve the CLI system prompt against the directory the run will read it
/// from. An explicit empty value stays empty so it keeps its override meaning.
fn resolve_system_prompt_path(path: &str, base: &Path) -> PathBuf {
    if path.is_empty() {
        PathBuf::new()
    } else {
        resolve_run_path(Path::new(path), base)
    }
}

/// Whether a resumed session's project-relative extensions remain trusted.
///
/// A stored `--toolbox` or `--mcp-config` path under the session project's
/// `.cake/` is authority-bearing. It must not start just because the resume ran
/// from a different checkout. A one-run trust flag covers only the invocation
/// directory; a different origin needs its own persistent trust record.
fn resumed_project_extensions_trusted(
    session_dir: &Path,
    invocation_dir: &Path,
    mode: ProjectConfigMode,
) -> bool {
    if !mode.loads_project_config() {
        return false;
    }
    if mode.one_run() && same_canonical_path(session_dir, invocation_dir) {
        return true;
    }
    ProjectConfigSnapshot::load(
        session_dir,
        &crate::config::config_dir(),
        ProjectConfigMode::Inspect,
    )
    .ok()
    .and_then(|snapshot| snapshot.evaluate().ok())
    .is_some_and(|report| report.allows_authority())
}

fn same_canonical_path(left: &Path, right: &Path) -> bool {
    std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf())
        == std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf())
}

fn restored_extension_paths(
    stored: &[PathBuf],
    filter: fn(Vec<PathBuf>, &[&Path]) -> Vec<PathBuf>,
    session_dir: &Path,
    invocation_dir: &Path,
    mode: ProjectConfigMode,
) -> (Vec<PathBuf>, bool) {
    let filtered = filter(stored.to_vec(), &[session_dir]);
    if filtered.len() == stored.len()
        || resumed_project_extensions_trusted(session_dir, invocation_dir, mode)
    {
        (stored.to_vec(), false)
    } else {
        (filtered, true)
    }
}

impl CmdRunner for CodingAssistant {
    async fn run(&self, data_dir: &DataDir, options: &CommandRunOptions<'_>) -> anyhow::Result<()> {
        if self.command.is_some() {
            return self.run_subcommand(data_dir, options).await;
        }
        Box::pin(self.run_agent(data_dir, options)).await
    }
}

impl CodingAssistant {
    fn run_context(
        &self,
        data_dir: &DataDir,
        project_config_mode: ProjectConfigMode,
    ) -> anyhow::Result<(RunMode, Option<Self>, PathBuf)> {
        let invocation_dir = std::env::current_dir()?;
        let run_mode = RunMode::from_cli(self)?;
        let restored = Self::restored_resume_config(&run_mode, data_dir)?;
        let effective = self.with_restored_resume_config(
            restored.as_ref(),
            &invocation_dir,
            project_config_mode,
        )?;
        Ok((run_mode, effective, invocation_dir))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "top-level run orchestration wires every session resource together"
    )]
    async fn run_agent(
        &self,
        data_dir: &DataDir,
        options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<()> {
        let (run_mode, effective, invocation_dir) =
            self.run_context(data_dir, options.project_config_mode)?;
        let this = effective.as_ref().unwrap_or(self);
        let (prepared, resources) = this
            .prepare_run_context(data_dir, options.project_config_mode)
            .await?;
        let task_id = uuid::Uuid::new_v4();
        let config_dir = crate::config::config_dir().join("cake");
        let cli_system_prompt = this
            .system_prompt
            .as_deref()
            .map(|path| resolve_system_prompt_path(path, &prepared.current_dir));
        let mut run_session = this.build_client_and_session(
            &run_mode,
            data_dir,
            prepared.current_dir.clone(),
            &config_dir,
            &resources.agents_files,
            &resources.loaded.models,
            &resources.loaded.images,
            resources.loaded.default_model.as_deref(),
            &resources.skill_catalog,
            &resources.tool_context,
            &resources.toolbox_tools,
            &resources.mcp_servers,
            resources.tools_enabled.as_deref(),
            task_id,
            cli_system_prompt.as_deref(),
            resources.loaded.system_prompt.as_deref(),
            &resources.loaded.skills,
            resources.project_config_mode.loads_project_config(),
        )?;
        run_session.session.run_config =
            Some(this.session_run_config(&prepared, &resources, &invocation_dir));

        run_session.agent = run_session
            .agent
            .with_policy_gates(&resources.loaded, this.no_gate)
            .await;

        Self::warn_unavailable_tools(
            resources.tools_enabled.as_deref(),
            &run_session.agent.tool_names(),
        );

        run_session.agent = run_session
            .agent
            .with_limits(&resources.loaded.limits)
            .with_image_policy(&resources.loaded.images);
        run_session.agent = Self::apply_compaction_policy(
            run_session.agent,
            &resources.loaded,
            &run_session.resolved,
        )?;
        run_session.attach_output_schema(prepared.output_schema.as_ref());
        run_session.attach_tool_choice(this.tool_choice);

        let session_start_source =
            HookSource::SessionStart(run_mode.session_start_source().to_owned());
        let effective_provider_identity = run_session.effective_provider_identity;
        let session_model_change_override = run_session.session_model_change_override;
        let session = run_session.session;
        let (client, session_writer) = Self::attach_persistence(
            run_session.agent,
            data_dir,
            &session,
            run_session.persistence,
        )?;
        let client = Self::attach_session_telemetry(
            client,
            data_dir,
            &session,
            &run_mode,
            this.output_format,
            &resources.trust,
            effective_provider_identity.as_ref(),
            session_model_change_override,
        );
        let hook_context = HookContext {
            session_id: session.id,
            task_id,
            transcript_path: run_mode
                .persists_session()
                .then(|| data_dir.session_path(session.id)),
            hook_event_sink: Self::hook_event_sink(session_writer, this.output_format),
            cwd: prepared.current_dir.clone(),
            model: client.model_name().to_string(),
        };
        let hook_output_limit = resources.loaded.limits.tool_limits.hook_output_limit;
        let (client, hook_runner) = if run_mode.is_read_only() {
            (client, None)
        } else {
            Self::attach_hooks(
                client,
                hook_context,
                hook_output_limit,
                &resources.project_hook_files,
            )?
        };
        let output = this.output_sink();
        let mut client = output.attach_callbacks(client);

        program_status::report(State::Working, None);

        let mut interrupts = InterruptListeners::install();

        let turn_start = Instant::now();

        let turn: Option<TurnResult> = tokio::select! {
            biased;
            result = async {
                Self::execute_agent_turn(
                    &mut client,
                    hook_runner.as_ref(),
                    output,
                    session_start_source,
                    &prepared.content,
                    prepared.output_schema.is_none(),
                )
                .await
            } => Some(match result {
                Ok(t) => t,
                Err(e) => {
                    Self::restore_post_turn_signal_state();
                    program_status::report(State::Error, Some(&sink_safe_error_text(&e)));
                    return Err(e);
                },
            }),
            () = interrupts.wait() => None,
        };

        let Some(turn) = turn else {
            return Self::handle_interrupt(&mut client, hook_runner.as_ref(), turn_start).await;
        };

        Self::restore_post_turn_signal_state();

        client.emit_session_summary_telemetry(
            turn.result.is_ok(),
            turn.duration_ms,
            turn.result.as_ref().err().map(sink_safe_error_text),
        );
        Self::report_turn_outcome(&turn.result);
        output.render_turn(
            turn,
            &client,
            &prepared.current_dir,
            data_dir,
            &session,
            run_mode.persists_session(),
        )?;

        Ok(())
    }
}

impl CodingAssistant {
    async fn run_subcommand(
        &self,
        data_dir: &DataDir,
        options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<()> {
        self.validate_subcommand_options()?;
        let Some(command) = self.command.as_ref() else {
            return Ok(());
        };
        command.run(data_dir, options).await
    }

    /// Restore SIGTERM's default termination now that the agent turn is
    /// over and the graceful-interrupt listener is no longer needed.
    ///
    /// Tokio's SIGTERM handler stays registered for the process lifetime,
    /// so without this a SIGTERM arriving during output rendering or
    /// teardown would be swallowed instead of terminating the process. On
    /// non-Unix there is no SIGTERM to restore.
    fn restore_post_turn_signal_state() {
        #[cfg(unix)]
        {
            // SAFETY: setting our own process's SIGTERM disposition is a
            // single libc call with no shared-state or aliasing concerns.
            unsafe {
                libc::signal(libc::SIGTERM, libc::SIG_DFL);
            }
        }
    }

    /// Handle a user interrupt (Ctrl-C or SIGTERM) during an agent turn.
    ///
    /// Reports `idle`, emits a `TaskComplete` record with an `Interrupted`
    /// outcome, writes the telemetry summary, and returns an `Interrupted`
    /// error that `main()` maps to exit code 130. Worktree cleanup is handled
    /// by [`WorktreeGuard`]'s `Drop`.
    async fn handle_interrupt(
        client: &mut Agent,
        hook_runner: Option<&Arc<HookRunner>>,
        turn_start: Instant,
    ) -> anyhow::Result<()> {
        let _second_interrupt = tokio::spawn(async {
            InterruptListeners::install().wait().await;
            std::process::exit(exit_code::code::INTERRUPTED.into());
        });

        program_status::report(State::Idle, Some("Interrupted by user"));

        let elapsed: u64 = turn_start
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);

        Self::run_session_end_hook(hook_runner, SessionEndReason::Interrupted).await;

        if let Err(e) = client.emit_task_complete_record(TaskOutcome::Interrupted, elapsed) {
            tracing::warn!(
                target: "cake",
                "Failed to emit interrupted TaskComplete record: {e}"
            );
        }

        client.emit_session_summary_telemetry(
            false,
            elapsed,
            Some("Interrupted by user".to_string()),
        );

        Err(Interrupted.into())
    }
}

/// Error returned when the user interrupts a run with Ctrl-C.
///
/// `main()` maps this to exit code 130 instead of classifying it through
/// the normal error pipeline.
#[derive(Debug)]
struct Interrupted;

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Interrupted by user")
    }
}

impl std::error::Error for Interrupted {}

/// The interruption listeners for an agent turn.
///
/// Tokio registers a signal's OS handler on the first poll of the listener it
/// returns, not when the future is created. Installing both listeners before
/// the turn starts keeps an interrupt that lands during the turn's startup on
/// the graceful path -- it closes the task with an interrupted `TaskComplete`
/// record -- instead of taking the default disposition, which exits without
/// one.
struct InterruptListeners {
    #[cfg(unix)]
    sigint: Option<Signal>,
    #[cfg(unix)]
    sigterm: Option<Signal>,
}

impl InterruptListeners {
    /// Install the OS handlers now, before the agent turn can start.
    fn install() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Self {
                sigint: signal(SignalKind::interrupt()).ok(),
                sigterm: signal(SignalKind::terminate()).ok(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// Wait for an interruption signal.
    ///
    /// Resolves on Ctrl-C (SIGINT) and, on Unix, also on SIGTERM, so
    /// supervisors that cancel cake with SIGTERM (such as the cake-repl
    /// runner) take the same graceful interruption path as Ctrl-C.
    async fn wait(&mut self) {
        #[cfg(unix)]
        {
            match (&mut self.sigint, &mut self.sigterm) {
                (Some(sigint), Some(sigterm)) => {
                    tokio::select! {
                        biased;
                        _ = sigint.recv() => {}
                        _ = sigterm.recv() => {}
                    }
                },
                (Some(sigint), None) => {
                    sigint.recv().await;
                },
                (None, Some(sigterm)) => {
                    sigterm.recv().await;
                },
                (None, None) => {
                    let _ = tokio::signal::ctrl_c().await.ok();
                },
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await.ok();
        }
    }
}

/// RAII guard that ensures a git worktree is cleaned up on early-exit paths.
///
/// On drop, restores the original working directory and removes the worktree
/// if it has no changes. This covers early failures, Ctrl-C interrupts, and
/// normal completion, so callers never need to manually clean up.
struct WorktreeGuard {
    inner: Option<worktree::Worktree>,
    original_dir: PathBuf,
}

impl WorktreeGuard {
    const fn new(inner: Option<worktree::Worktree>, original_dir: PathBuf) -> Self {
        Self {
            inner,
            original_dir,
        }
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        let Some(ref wt) = self.inner else { return };

        if let Err(e) = std::env::set_current_dir(&self.original_dir) {
            tracing::warn!(
                "Failed to restore original directory '{}': {e}",
                self.original_dir.display()
            );
        }

        match worktree::has_changes(&wt.path) {
            Ok(false) => {
                eprintln!("No changes in worktree '{}', removing.", wt.name);
                if let Err(e) = worktree::remove(&self.original_dir, &wt.name, false) {
                    tracing::warn!("Failed to clean up worktree '{}': {e}", wt.name);
                }
            },
            Ok(true) => {
                eprintln!(
                    "Worktree '{}' has changes, keeping at {}",
                    wt.name,
                    wt.path.display()
                );
            },
            Err(e) => {
                eprintln!(
                    "Could not determine state of worktree '{}', keeping at {}: {e}",
                    wt.name,
                    wt.path.display()
                );
            },
        }
    }
}

fn finish_run(result: anyhow::Result<()>) -> std::process::ExitCode {
    match result {
        Ok(()) => std::process::ExitCode::from(exit_code::code::SUCCESS),
        Err(error) if CliOutputSink::is_closed_output(&error) => {
            std::process::ExitCode::from(exit_code::code::SUCCESS)
        },
        Err(error) if error.is::<Interrupted>() => {
            std::process::ExitCode::from(exit_code::code::INTERRUPTED)
        },
        Err(error) => {
            CliOutputSink::write_error(&error);
            exit_code::classify(&error)
        },
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args = match CodingAssistant::try_parse() {
        Ok(a) => a,
        Err(e) => {
            #[expect(
                clippy::unused_result_ok,
                reason = "best-effort error printing; we already exit non-zero"
            )]
            e.print().ok();
            let exit = if e.exit_code() == 0 {
                std::process::ExitCode::from(exit_code::code::SUCCESS)
            } else {
                std::process::ExitCode::from(exit_code::code::INPUT_ERROR)
            };
            return exit;
        },
    };

    let data_dir = match DataDir::new() {
        Ok(d) => d,
        Err(e) => {
            CliOutputSink::write_error(&e);
            return exit_code::classify(&e);
        },
    };

    _ = logger::configure(&data_dir.get_cache_dir());

    info!("data dir: {}", data_dir.get_cache_dir().display());

    let options = CommandRunOptions {
        no_gate: args.no_gate,
        model: args.model.as_deref(),
        profile: args.profile.as_deref(),
        output_format: args.output_format,
        project_config_mode: match args.resolve_project_config_mode() {
            Ok(mode) => mode,
            Err(error) => return finish_run(Err(error.into())),
        },
    };
    finish_run(args.run(&data_dir, &options).await)
}
