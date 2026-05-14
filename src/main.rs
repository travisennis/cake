//! cake - AI coding assistant CLI

mod cli;
mod clients;
mod config;
mod exit_code;
mod hooks;
mod logger;
mod models;
mod prompts;
mod time_format;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cli::CmdRunner;
use crate::clients::{Agent, ConversationItem, TaskOutcome, ToolContext};
use crate::config::settings::LoadedSettings;
use crate::config::skills::Skill;
use crate::config::{
    AgentsFile, DataDir, DiagnosticLevel, HookSource, HooksLoader, ModelConfig, ModelDefinition,
    ReasoningEffort, ResolvedModelConfig, Session, SettingsLoader, SkillCatalog, discover_skills,
    discover_skills_with_paths, parse_skill_path_list, worktree,
};
use crate::hooks::{HookContext, HookRunner};
use crate::models::{Message, Role};
use crate::prompts::build_initial_prompt_messages;
use crate::time_format::{format_duration_tenths, format_seconds_tenths};
use clap::{ArgGroup, Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use tracing::info;

/// Output format for the response
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
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
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(group(
    ArgGroup::new("session_mode")
        .args(["continue_session", "resume", "fork", "no_session"])
        .multiple(false)
))]
struct CodingAssistant {
    /// The prompt to send to the AI (use `-` to read from stdin)
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Sets the max tokens value
    #[arg(long)]
    pub max_tokens: Option<u32>,

    /// Output format for the response (text, json, or stream-json)
    #[arg(long, value_enum, default_value = "text")]
    pub output_format: OutputFormat,

    /// Continue the most recent session for this directory
    #[arg(long = "continue")]
    pub continue_session: bool,

    /// Resume a specific session by UUID.
    #[arg(long, value_name = "UUID")]
    pub resume: Option<String>,

    /// Fork a session: copy its history into a new session with a fresh ID.
    /// Use without a value to fork the latest session, or provide a UUID.
    #[arg(long, num_args = 0..=1, default_missing_value = "", value_name = "UUID")]
    pub fork: Option<String>,

    /// Do not save the session to disk
    #[arg(long)]
    pub no_session: bool,

    /// Run in an isolated git worktree (optionally provide a name)
    #[arg(short, long, num_args = 0..=1, default_missing_value = "", value_name = "NAME")]
    pub worktree: Option<String>,

    /// Select a model by name from settings.toml
    #[arg(long)]
    pub model: Option<String>,

    /// Apply a named behavior profile from settings.toml
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,

    /// Override reasoning effort level (none, low, medium, high, xhigh)
    #[arg(long, value_name = "EFFORT")]
    pub reasoning_effort: Option<ReasoningEffort>,

    /// Override reasoning token budget
    #[arg(long, value_name = "TOKENS")]
    pub reasoning_budget: Option<u32>,

    /// Add a directory to the sandbox config (read-only access). Can be repeated.
    #[arg(long, value_name = "DIR")]
    pub add_dir: Vec<String>,

    /// Disable all skills for this session
    #[arg(long)]
    pub no_skills: bool,

    /// Only load specific skills (comma-separated list of skill names)
    #[arg(long, value_name = "NAMES")]
    pub skills: Option<String>,
}

struct RunSession {
    agent: Agent,
    session: Session,
    storage: SessionStorage,
    seed_records: Option<Vec<crate::clients::SessionRecord>>,
}

struct PreparedRun {
    original_dir: PathBuf,
    current_dir: PathBuf,
    additional_dirs: Vec<PathBuf>,
    worktree: Option<worktree::Worktree>,
    content: String,
}

struct RunResources {
    loaded: LoadedSettings,
    agents_files: Vec<AgentsFile>,
    skill_catalog: SkillCatalog,
    tool_context: Arc<ToolContext>,
}

struct TurnResult {
    result: anyhow::Result<Option<Message>>,
    duration_ms: u64,
}

#[derive(Clone, Copy)]
struct CliOutputSink {
    format: OutputFormat,
}

impl CliOutputSink {
    const fn new(format: OutputFormat) -> Self {
        Self { format }
    }

    fn attach_callbacks(self, mut client: Agent) -> (Agent, Option<ProgressBar>) {
        if self.format == OutputFormat::StreamJson {
            client = client.with_streaming_json(Self::write_stream_record);
        }

        match self.format {
            OutputFormat::Text => {
                let (client, spinner) = CodingAssistant::with_text_progress(client);
                (client, Some(spinner))
            },
            OutputFormat::StreamJson | OutputFormat::Json => (client, None),
        }
    }

    fn finish_progress(self, spinner: Option<ProgressBar>, duration_ms: u64, client: &Agent) {
        if self.format == OutputFormat::Text
            && let Some(spinner) = spinner
        {
            let summary = format_done_summary(duration_ms, client);
            spinner.finish_with_message(format!("Done: {summary}"));
        }
    }

    fn render_turn(
        self,
        turn: TurnResult,
        client: &Agent,
        current_dir: &Path,
        data_dir: &DataDir,
        session: &Session,
    ) -> anyhow::Result<()> {
        let TurnResult {
            result,
            duration_ms,
        } = turn;

        match self.format {
            OutputFormat::Text => Self::render_text_result(result),
            OutputFormat::Json => {
                let json = Self::turn_result_json(
                    &result,
                    duration_ms,
                    client,
                    current_dir,
                    data_dir,
                    session,
                );
                Self::write_json_value(&json)?;
                result.map(|_| ())
            },
            OutputFormat::StreamJson => Ok(()),
        }
    }

    fn render_text_result(result: anyhow::Result<Option<Message>>) -> anyhow::Result<()> {
        let response = result?;
        if let Some(response_msg) = response {
            Self::write_text_response(&response_msg.content);
        } else {
            Self::write_warning("No response received from the model. The task may be incomplete.");
        }
        Ok(())
    }

    fn turn_result_json(
        result: &anyhow::Result<Option<Message>>,
        duration_ms: u64,
        client: &Agent,
        current_dir: &Path,
        data_dir: &DataDir,
        session: &Session,
    ) -> serde_json::Value {
        let mut json = serde_json::json!({
            "session_id": client.session_id().to_string(),
            "usage": client.total_usage(),
            "cwd": current_dir.to_string_lossy(),
            "session_file": data_dir.session_path(session.id).to_string_lossy(),
            "turns": client.turn_count(),
            "elapsed_time": duration_ms,
        });

        match result {
            Ok(response_msg) => {
                let result_text = response_msg.as_ref().map_or("", |m| m.content.as_str());
                json["result"] = serde_json::json!(result_text);
            },
            Err(e) => {
                json["result"] = serde_json::Value::Null;
                json["error"] = serde_json::json!(e.to_string());
            },
        }

        json
    }

    fn write_stream_record(json: &str) {
        println!("{json}");
    }

    fn write_text_response(content: &str) {
        println!("{content}");
    }

    fn write_json_value(value: &serde_json::Value) -> anyhow::Result<()> {
        println!("{}", serde_json::to_string(value)?);
        Ok(())
    }

    fn write_warning(message: &str) {
        eprintln!("Warning: {message}");
    }

    fn write_error(error: &anyhow::Error) {
        eprintln!("Error: {error}");
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RunMode {
    NewSession,
    Ephemeral,
    ContinueLatest,
    Resume { session_id: uuid::Uuid },
    ForkLatest,
    Fork { session_id: uuid::Uuid },
}

impl RunMode {
    fn from_cli(args: &CodingAssistant) -> anyhow::Result<Self> {
        if args.no_session {
            return Ok(Self::Ephemeral);
        }
        if args.continue_session {
            return Ok(Self::ContinueLatest);
        }
        if let Some(session_id) = args.resume.as_deref() {
            let id = uuid::Uuid::parse_str(session_id).map_err(|_e| {
                anyhow::anyhow!(
                    "Invalid session UUID '{session_id}'. Expected format: e.g., 550e8400-e29b-41d4-a716-446655440000"
                )
            })?;
            return Ok(Self::Resume { session_id: id });
        }
        if let Some(fork_id) = args.fork.as_deref() {
            if fork_id.is_empty() {
                return Ok(Self::ForkLatest);
            }
            let id = uuid::Uuid::parse_str(fork_id).map_err(|_e| {
                anyhow::anyhow!(
                    "Invalid session UUID '{fork_id}'. Expected format: e.g., 550e8400-e29b-41d4-a716-446655440000"
                )
            })?;
            return Ok(Self::Fork { session_id: id });
        }

        Ok(Self::NewSession)
    }

    const fn persists_session(&self) -> bool {
        !matches!(self, Self::Ephemeral)
    }

    const fn session_start_source(&self) -> &'static str {
        match self {
            Self::ForkLatest | Self::Fork { .. } => "fork",
            Self::ContinueLatest | Self::Resume { .. } => "resume",
            Self::NewSession | Self::Ephemeral => "startup",
        }
    }
}

enum SessionStorage {
    New,
    Append,
}

impl CodingAssistant {
    /// Maximum size in bytes for stdin input before it is rejected.
    const MAX_STDIN_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

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

    /// Resolve the effective `ModelConfig`, applying CLI overrides.
    fn resolve_model_config(
        &self,
        models: &HashMap<String, ModelDefinition>,
        default_model: Option<&str>,
    ) -> anyhow::Result<ModelConfig> {
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

        // Validate model name format
        if let Err(e) = ModelDefinition::validate_name(model_name) {
            anyhow::bail!(
                "Invalid model name '{model_name}': {e}. Model names must contain only lowercase letters, numbers, and hyphens."
            );
        }

        // Look up the model in settings
        let mut config = if let Some(def) = models.get(model_name) {
            def.to_model_config()
        } else {
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

        // Apply CLI overrides
        if let Some(max_tokens) = self.max_tokens {
            config.max_output_tokens = Some(max_tokens);
        }
        if let Some(effort) = self.reasoning_effort {
            config.reasoning_effort = Some(effort);
        }
        if let Some(budget) = self.reasoning_budget {
            config.reasoning_max_tokens = Some(budget);
        }

        Ok(config)
    }

    /// Resolve the model for a session restore (--continue, --resume, --fork).
    ///
    /// Policy (per the plan):
    /// - If `--model` is explicitly provided and the session has a stored model that
    ///   differs from it, error out with a clear message.
    /// - If `--model` is explicitly provided and matches the session model (or session
    ///   has no model), use the explicitly provided model.
    /// - If `--model` is not provided and the session has a stored model, use the
    ///   session model.
    /// - If `--model` is not provided and the session has no stored model, fall back
    ///   to default model resolution.
    fn resolve_model_for_session(
        &self,
        models: &HashMap<String, ModelDefinition>,
        default_model: Option<&str>,
        session_model: Option<&str>,
    ) -> anyhow::Result<ResolvedModelConfig> {
        let cli_model_explicit = self.model.is_some();

        if cli_model_explicit {
            // User explicitly passed --model. Resolve it and check against session.
            let config = self.resolve_model_config(models, default_model)?;
            let resolved = ResolvedModelConfig::resolve(config)?;
            let resolved_model = &resolved.model_config.model;

            if let Some(sm) = session_model
                && sm != resolved_model
            {
                anyhow::bail!(
                    "Session model mismatch: session uses '{sm}' but --model resolves to '{resolved_model}'. \
                     Use --model {sm} to continue with the session's model, or start a new session."
                );
            }

            Ok(resolved)
        } else if let Some(sm) = session_model {
            // No --model, but session has a stored model. Use it.
            // Look up the model name in settings to get provider config.
            // Try by name first (for sessions that store the config name),
            // then by model identifier (for backward compatibility with older
            // sessions that stored the API model string).
            let def = models
                .get(sm)
                .or_else(|| models.values().find(|d| d.model == sm));
            if let Some(def) = def {
                let resolved = ResolvedModelConfig::resolve(def.to_model_config())?;
                let resolved = self.apply_cli_overrides(resolved);
                Ok(resolved)
            } else {
                anyhow::bail!(
                    "Session model '{sm}' is not configured in settings.toml. \
                     Add a [[models]] entry for '{sm}' to continue this session, \
                     or start a new session."
                );
            }
        } else {
            // No --model, no session model. Fall back to default.
            let config = self.resolve_model_config(models, default_model)?;
            let resolved = ResolvedModelConfig::resolve(config)?;
            Ok(resolved)
        }
    }

    /// Apply CLI overrides (`max_tokens`, `reasoning_effort`, `reasoning_budget`) to a
    /// resolved model config.
    const fn apply_cli_overrides(&self, mut resolved: ResolvedModelConfig) -> ResolvedModelConfig {
        if let Some(max_tokens) = self.max_tokens {
            resolved.model_config.max_output_tokens = Some(max_tokens);
        }
        if let Some(effort) = self.reasoning_effort {
            resolved.model_config.reasoning_effort = Some(effort);
        }
        if let Some(budget) = self.reasoning_budget {
            resolved.model_config.reasoning_max_tokens = Some(budget);
        }
        resolved
    }

    /// Build a map of skill file paths to skills for activation deduplication.
    fn skill_locations(skill_catalog: &SkillCatalog) -> HashMap<PathBuf, Skill> {
        skill_catalog
            .skills
            .iter()
            .map(|s| {
                let skill = skill_catalog
                    .get_skill_by_location(&s.location)
                    .unwrap_or(s);
                let location = s
                    .location
                    .canonicalize()
                    .unwrap_or_else(|_| s.location.clone());
                (location, skill.clone())
            })
            .collect()
    }

    /// Convert a restored session into the agent/session pair used for a continued run.
    fn restored_client_and_session(
        restored: Session,
        resolved: ResolvedModelConfig,
        initial_messages: &[(Role, String)],
        skill_locations: &HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        task_id: uuid::Uuid,
    ) -> anyhow::Result<RunSession> {
        let messages = restored.messages();
        let prior_skills = restored.activated_skills();

        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_session_id(restored.id)
            .with_task_id(task_id)
            .with_tool_context(tool_context)
            .with_history(messages)?
            .with_skill_locations(skill_locations.clone())
            .with_activated_skills(prior_skills);
        let mut session = Session::new(restored.id, restored.working_dir);
        session.model = Some(resolved.model_config.model);
        Ok(RunSession {
            agent,
            session,
            storage: SessionStorage::Append,
            seed_records: None,
        })
    }

    /// Build the agent/session pair for a new run using the agent-generated session id.
    fn new_client_and_session(
        resolved: ResolvedModelConfig,
        current_dir: PathBuf,
        initial_messages: &[(Role, String)],
        skill_locations: HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        task_id: uuid::Uuid,
    ) -> RunSession {
        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_task_id(task_id)
            .with_tool_context(tool_context)
            .with_skill_locations(skill_locations);
        let new_id = agent.session_id();
        info!(target: "cake", "New session: {new_id}");
        let mut session = Session::new(new_id, current_dir);
        session.model = Some(resolved.model_config.model);
        session.system_prompt = initial_messages.first().map(|(_, content)| content.clone());
        RunSession {
            agent,
            session,
            storage: SessionStorage::New,
            seed_records: None,
        }
    }

    /// Build the agent/session pair for a forked run using a fresh agent session id.
    fn forked_client_and_session(
        restored: &Session,
        resolved: ResolvedModelConfig,
        current_dir: PathBuf,
        initial_messages: &[(Role, String)],
        skill_locations: HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        task_id: uuid::Uuid,
    ) -> anyhow::Result<RunSession> {
        let prior_skills = restored.activated_skills();
        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_task_id(task_id)
            .with_tool_context(tool_context)
            .with_history(restored.messages())?
            .with_skill_locations(skill_locations)
            .with_activated_skills(prior_skills);
        let new_id = agent.session_id();
        let seed_records: Vec<_> = restored
            .records
            .iter()
            .filter_map(|record| match record {
                record if record.to_conversation_item().is_some() => Some(record.clone()),
                crate::clients::SessionRecord::SkillActivated {
                    task_id,
                    timestamp,
                    name,
                    path,
                    ..
                } => Some(crate::clients::SessionRecord::SkillActivated {
                    session_id: new_id.to_string(),
                    task_id: task_id.clone(),
                    timestamp: *timestamp,
                    name: name.clone(),
                    path: path.clone(),
                }),
                _ => None,
            })
            .collect();
        info!(target: "cake", "New forked session: {new_id}");
        let mut session = Session::new(new_id, current_dir);
        session.model = Some(resolved.model_config.model);
        session.system_prompt = initial_messages.first().map(|(_, content)| content.clone());
        Ok(RunSession {
            agent,
            session,
            storage: SessionStorage::New,
            seed_records: Some(seed_records),
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "session construction naturally requires many parameters"
    )]
    fn build_client_and_session(
        &self,
        run_mode: &RunMode,
        data_dir: &DataDir,
        current_dir: PathBuf,
        agents_files: &[AgentsFile],
        models: &HashMap<String, ModelDefinition>,
        default_model: Option<&str>,
        skill_catalog: &SkillCatalog,
        tool_context: &Arc<ToolContext>,
        task_id: uuid::Uuid,
    ) -> anyhow::Result<RunSession> {
        let initial_messages =
            build_initial_prompt_messages(&current_dir, agents_files, skill_catalog);
        let skill_locations = Self::skill_locations(skill_catalog);

        match run_mode {
            RunMode::ContinueLatest => {
                info!(target: "cake", "Continuing latest session for directory: {}", current_dir.display());
                let Some(restored) = data_dir.load_latest_session(&current_dir)? else {
                    if let Some(latest) = data_dir.load_latest_session_any_directory()? {
                        anyhow::bail!(
                            "Cannot continue: latest session was created in '{}' but current directory is '{}'. Run from the original directory or start a new session.",
                            latest.working_dir.display(),
                            current_dir.display()
                        );
                    }
                    anyhow::bail!("No previous session found for this directory");
                };
                info!(target: "cake", "Continuing session: {}", restored.id);
                let resolved = self.resolve_model_for_session(
                    models,
                    default_model,
                    restored.model.as_deref(),
                )?;
                Self::restored_client_and_session(
                    restored,
                    resolved,
                    &initial_messages,
                    &skill_locations,
                    Arc::clone(tool_context),
                    task_id,
                )
            },
            RunMode::Resume { session_id } => {
                let restored = data_dir
                    .load_session(*session_id)?
                    .ok_or_else(|| anyhow::anyhow!("Session {session_id} not found"))?;
                info!(target: "cake", "Resumed session: {}", restored.id);

                let resolved = self.resolve_model_for_session(
                    models,
                    default_model,
                    restored.model.as_deref(),
                )?;
                Self::restored_client_and_session(
                    restored,
                    resolved,
                    &initial_messages,
                    &skill_locations,
                    Arc::clone(tool_context),
                    task_id,
                )
            },
            RunMode::ForkLatest | RunMode::Fork { .. } => {
                info!(target: "cake", "Forking session");
                let restored = match run_mode {
                    RunMode::ForkLatest => {
                        data_dir.load_latest_session(&current_dir)?.ok_or_else(|| {
                            anyhow::anyhow!("No previous session found for this directory")
                        })?
                    },
                    RunMode::Fork { session_id } => data_dir
                        .load_session(*session_id)?
                        .ok_or_else(|| anyhow::anyhow!("Session {session_id} not found"))?,
                    _ => unreachable!("fork arm only handles fork modes"),
                };

                info!(target: "cake", "Forking from session: {}", restored.id);
                let resolved = self.resolve_model_for_session(
                    models,
                    default_model,
                    restored.model.as_deref(),
                )?;
                Self::forked_client_and_session(
                    &restored,
                    resolved,
                    current_dir,
                    &initial_messages,
                    skill_locations,
                    Arc::clone(tool_context),
                    task_id,
                )
            },
            RunMode::NewSession | RunMode::Ephemeral => {
                let resolved = ResolvedModelConfig::resolve(
                    self.resolve_model_config(models, default_model)?,
                )?;
                Ok(Self::new_client_and_session(
                    resolved,
                    current_dir,
                    &initial_messages,
                    skill_locations,
                    Arc::clone(tool_context),
                    task_id,
                ))
            },
        }
    }

    /// Attach text-mode progress reporting to the agent and return its spinner.
    fn with_text_progress(client: Agent) -> (Agent, ProgressBar) {
        let spinner = ProgressBar::new_spinner();
        let style = ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner());
        spinner.set_style(style);
        spinner.enable_steady_tick(Duration::from_millis(80));
        spinner.set_message("Thinking...");

        let spinner_clone = spinner.clone();
        let retry_spinner = spinner.clone();
        let client = client.with_progress_callback(move |item| {
            let msg = format_spinner_message(item);
            if let Some(msg) = msg {
                spinner_clone.set_message(msg);
            }
        });
        let client = client.with_retry_callback(move |status| {
            retry_spinner.set_message(format_retry_message(status));
        });

        (client, spinner)
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

    /// Clean up a worktree after the session ends.
    fn cleanup_worktree(wt: &worktree::Worktree, original_dir: &std::path::Path) {
        if let Err(e) = std::env::set_current_dir(original_dir) {
            tracing::warn!(
                "Failed to restore original directory '{}': {e}",
                original_dir.display()
            );
        }

        match worktree::has_changes(&wt.path) {
            Ok(false) => {
                eprintln!("No changes in worktree '{}', removing.", wt.name);
                if let Err(e) = worktree::remove(original_dir, &wt.name, false) {
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
                tracing::warn!(
                    "Could not check worktree '{}' for changes, keeping it: {e}",
                    wt.name
                );
            },
        }
    }

    /// Resolve `--add-dir` values against the startup directory.
    fn resolve_additional_dirs(&self, base_dir: &Path) -> Vec<PathBuf> {
        self.add_dir
            .iter()
            .filter_map(|dir| {
                let path = PathBuf::from(dir);
                let path_to_check = if path.is_absolute() {
                    path.clone()
                } else {
                    base_dir.join(&path)
                };
                if path_to_check.exists() && path_to_check.is_dir() {
                    Some(path)
                } else {
                    tracing::warn!(
                        "--add-dir path '{dir}' does not exist or is not a directory, ignoring"
                    );
                    None
                }
            })
            .collect()
    }

    fn prepare_run(&self) -> anyhow::Result<PreparedRun> {
        let original_dir = std::env::current_dir()?;
        let additional_dirs = self.resolve_additional_dirs(&original_dir);
        let worktree = self.setup_worktree(&original_dir)?;

        let stdin_content = Self::read_stdin_content()?;
        let content = Self::build_content(self.prompt.as_deref(), stdin_content)?;

        let current_dir = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("Failed to get current directory: {e}"))?;

        Ok(PreparedRun {
            original_dir,
            current_dir,
            additional_dirs,
            worktree,
            content,
        })
    }

    fn load_settings(&self, current_dir: &Path) -> anyhow::Result<LoadedSettings> {
        let loaded = if let Some(profile) = self.profile.as_deref() {
            SettingsLoader::load_with_profile(Some(current_dir), Some(profile))?
        } else {
            SettingsLoader::load(Some(current_dir))?
        };
        Ok(loaded)
    }

    fn load_run_resources(
        &self,
        data_dir: &DataDir,
        current_dir: &Path,
        additional_dirs: Vec<PathBuf>,
    ) -> anyhow::Result<RunResources> {
        let loaded = self.load_settings(current_dir)?;
        let agents_files = data_dir.read_agents_files(current_dir);

        let skill_config = SettingsLoader::resolve_skill_config(
            self.no_skills,
            self.skills.as_deref(),
            &loaded.skills,
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

        let settings_dirs = Self::valid_settings_dirs(&loaded);
        let tool_context = ToolContext::new(
            current_dir.to_path_buf(),
            additional_dirs,
            skill_base_dirs,
            settings_dirs,
        );

        Self::log_skill_diagnostics(&skill_catalog);

        Ok(RunResources {
            loaded,
            agents_files,
            skill_catalog,
            tool_context: Arc::new(tool_context),
        })
    }

    fn valid_settings_dirs(loaded: &LoadedSettings) -> Vec<PathBuf> {
        loaded
            .directories
            .iter()
            .map(PathBuf::from)
            .filter(|p| {
                if p.exists() && p.is_dir() {
                    true
                } else {
                    tracing::warn!(
                        "settings.toml directory '{}' does not exist or is not a directory, ignoring",
                        p.display()
                    );
                    false
                }
            })
            .collect()
    }

    fn log_skill_diagnostics(skill_catalog: &SkillCatalog) {
        for diagnostic in &skill_catalog.diagnostics {
            match diagnostic.level {
                DiagnosticLevel::Warning => {
                    tracing::warn!(
                        "Skill diagnostic ({}): {}",
                        diagnostic.file.display(),
                        diagnostic.message
                    );
                },
                DiagnosticLevel::Error => {
                    tracing::error!(
                        "Skill diagnostic ({}): {}",
                        diagnostic.file.display(),
                        diagnostic.message
                    );
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

    fn attach_persistence(
        mut client: Agent,
        data_dir: &DataDir,
        session: &Session,
        storage: &SessionStorage,
        persists_session: bool,
    ) -> anyhow::Result<Agent> {
        if !persists_session {
            return Ok(client);
        }

        let mut file = match storage {
            SessionStorage::New => data_dir.create_session_file(session, client.tool_names())?,
            SessionStorage::Append => data_dir.open_session_for_append(session.id)?,
        };
        client = client.with_persist_callback(move |record| {
            crate::config::Session::append_record(&mut file, record)
        });
        Ok(client)
    }

    fn attach_hooks(
        mut client: Agent,
        data_dir: &DataDir,
        current_dir: &Path,
        session: &Session,
        run_mode: &RunMode,
        task_id: uuid::Uuid,
    ) -> anyhow::Result<(Agent, Option<Arc<HookRunner>>)> {
        let hooks = HooksLoader::load(current_dir)?;

        if hooks.is_empty() {
            return Ok((client, None));
        }

        let runner = Arc::new(HookRunner::new(
            hooks,
            HookContext {
                session_id: session.id,
                task_id,
                transcript_path: run_mode
                    .persists_session()
                    .then(|| data_dir.session_path(session.id)),
                cwd: current_dir.to_path_buf(),
                model: client.model_name().to_string(),
            },
        ));
        client = client.with_hook_runner(Arc::clone(&runner));
        Ok((client, Some(runner)))
    }

    const fn output_sink(&self) -> CliOutputSink {
        CliOutputSink::new(self.output_format)
    }

    async fn execute_agent_turn(
        client: &mut Agent,
        hook_runner: Option<&Arc<HookRunner>>,
        session_start_source: HookSource,
        content: &str,
    ) -> anyhow::Result<TurnResult> {
        let msg = Message {
            role: Role::User,
            content: content.to_string(),
        };

        let start = Instant::now();

        if let Some(runner) = hook_runner {
            let contexts = runner.session_start(&session_start_source, content).await?;
            client.append_developer_context(contexts);
            let contexts = runner.user_prompt_submit(content).await?;
            client.append_developer_context(contexts);
        }
        client.emit_prompt_context_records()?;
        client.emit_task_start_record()?;

        let result = client.send(msg).await;
        let duration_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

        match &result {
            Ok(response_msg) => {
                let result_text = response_msg.as_ref().map(|m| m.content.clone());
                if let Some(runner) = hook_runner
                    && let Some(context) = runner.stop(result_text.as_deref()).await?
                {
                    tracing::info!(target: "cake::hooks", additional_context = %context, "Stop hook returned additional context");
                }
                client.emit_task_complete_record(
                    TaskOutcome::Success {
                        result: result_text,
                    },
                    duration_ms,
                )?;
            },
            Err(e) => {
                if let Some(runner) = hook_runner {
                    runner.error_occurred(e).await?;
                }
                client.emit_task_complete_record(
                    TaskOutcome::ErrorDuringExecution {
                        error: e.to_string(),
                    },
                    duration_ms,
                )?;
            },
        }

        Ok(TurnResult {
            result,
            duration_ms,
        })
    }
}

impl CmdRunner for CodingAssistant {
    async fn run(&self, data_dir: &DataDir) -> anyhow::Result<()> {
        let prepared = self.prepare_run()?;
        let resources =
            self.load_run_resources(data_dir, &prepared.current_dir, prepared.additional_dirs)?;
        let task_id = uuid::Uuid::new_v4();
        let run_mode = RunMode::from_cli(self)?;
        let mut run_session = self.build_client_and_session(
            &run_mode,
            data_dir,
            prepared.current_dir.clone(),
            &resources.agents_files,
            &resources.loaded.models,
            resources.loaded.default_model.as_deref(),
            &resources.skill_catalog,
            &resources.tool_context,
            task_id,
        )?;

        // For fork mode: create the session file and write seed records
        // upfront, converting the fork into a normal append scenario.
        if let Some(seed_records) = run_session.seed_records.take() {
            let mut file = data_dir
                .create_session_file(&run_session.session, run_session.agent.tool_names())?;
            crate::config::Session::append_records(&mut file, &seed_records)?;
            run_session.storage = SessionStorage::Append;
        }

        let session_start_source =
            HookSource::SessionStart(run_mode.session_start_source().to_owned());
        let session = run_session.session;
        let client = Self::attach_persistence(
            run_session.agent,
            data_dir,
            &session,
            &run_session.storage,
            run_mode.persists_session(),
        )?;
        let (client, hook_runner) = Self::attach_hooks(
            client,
            data_dir,
            &prepared.current_dir,
            &session,
            &run_mode,
            task_id,
        )?;
        let output = self.output_sink();
        let (mut client, spinner) = output.attach_callbacks(client);

        let turn = Self::execute_agent_turn(
            &mut client,
            hook_runner.as_ref(),
            session_start_source,
            &prepared.content,
        )
        .await?;
        output.finish_progress(spinner, turn.duration_ms, &client);
        output.render_turn(turn, &client, &prepared.current_dir, data_dir, &session)?;

        if let Some(ref wt) = prepared.worktree {
            Self::cleanup_worktree(wt, &prepared.original_dir);
        }

        Ok(())
    }
}

/// Format a completion summary with elapsed time, turns, and token usage.
fn format_done_summary(duration_ms: u64, client: &Agent) -> String {
    let secs = format_seconds_tenths(u128::from(duration_ms));
    let turns = client.turn_count();
    let usage = client.total_usage();
    let input_tokens = usage.input_tokens;
    let output_tokens = usage.output_tokens;
    let cached_reads_tokens = usage.input_tokens_details.cached_tokens;
    format!(
        "session {}, {secs}s, {turns} turns, {input_tokens} input tokens, {cached_reads_tokens} cached reads, {output_tokens} output tokens",
        client.session_id()
    )
}

/// Format a conversation item as a short spinner message for normal mode.
///
/// Returns `Some(message)` for items worth showing, `None` otherwise.
fn format_spinner_message(item: &ConversationItem) -> Option<String> {
    match item {
        ConversationItem::FunctionCall {
            name, arguments, ..
        } => {
            let summary = clients::summarize_tool_args(name, arguments);
            Some(format!("{name}: {summary}"))
        },
        ConversationItem::Reasoning { .. } => Some("Thinking...".to_string()),
        ConversationItem::Message { role, .. } if *role == Role::Assistant => {
            Some("Responding...".to_string())
        },
        _ => None,
    }
}

fn format_retry_message(status: &crate::clients::retry::RetryStatus) -> String {
    if status.reason == crate::clients::retry::RetryReason::ContextOverflow {
        return format!(
            "Retrying once with {} after context overflow",
            status.detail
        );
    }

    let delay = format_duration_tenths(status.delay);
    format!(
        "Retrying in {delay}s after {} (attempt {}/{})",
        status.detail, status.attempt, status.max_retries
    )
}

fn main() -> std::process::ExitCode {
    let data_dir = match DataDir::new() {
        Ok(d) => d,
        Err(e) => {
            CliOutputSink::write_error(&e);
            return exit_code::classify(&e);
        },
    };

    _ = logger::configure(&data_dir.get_cache_dir());

    info!("data dir: {}", data_dir.get_cache_dir().display());

    let args = match CodingAssistant::try_parse() {
        Ok(a) => a,
        Err(e) => {
            // Print the clap error (includes --help and --version output).
            // For --help/--version, clap returns exit_code() == 0 and the
            // formatted output goes to stdout. For actual errors (bad flags,
            // missing required args), it goes to stderr with exit_code() != 0.
            _ = e.print();
            let exit = if e.exit_code() == 0 {
                std::process::ExitCode::from(exit_code::code::SUCCESS)
            } else {
                std::process::ExitCode::from(exit_code::code::INPUT_ERROR)
            };
            return exit;
        },
    };

    // Set up the Tokio runtime and run the async command
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            let err = anyhow::anyhow!("Failed to initialize Tokio runtime: {e}");
            CliOutputSink::write_error(&err);
            return exit_code::classify(&err);
        },
    };

    let result = rt.block_on(args.run(&data_dir));

    match result {
        Ok(()) => std::process::ExitCode::from(exit_code::code::SUCCESS),
        Err(e) => {
            CliOutputSink::write_error(&e);
            exit_code::classify(&e)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::types::FunctionCallOutputData;
    use crate::config::model::ApiType;

    fn test_resolved_model_config() -> ResolvedModelConfig {
        ResolvedModelConfig {
            model_config: ModelConfig {
                model: "test-model".to_string(),
                api_type: ApiType::ChatCompletions,
                base_url: "https://api.example.com".to_string(),
                api_key_env: "TEST_API_KEY".to_string(),
                temperature: None,
                top_p: None,
                max_output_tokens: None,
                reasoning_effort: None,
                reasoning_summary: None,
                reasoning_max_tokens: None,
                providers: vec![],
            },
            api_key: "test-key".to_string(),
        }
    }

    fn session_with_skill_records() -> Session {
        let mut session = Session::new(
            uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"),
            PathBuf::from("/work"),
        );
        session.records = vec![
            crate::clients::SessionRecord::FunctionCallOutput(FunctionCallOutputData {
                call_id: "call-1".to_string(),
                output: "echoed text: Skill 'fake-skill' activated".to_string(),
                timestamp: None,
            }),
            crate::clients::SessionRecord::SkillActivated {
                session_id: session.id.to_string(),
                task_id: "task-1".to_string(),
                timestamp: chrono::Utc::now(),
                name: "real-skill".to_string(),
                path: PathBuf::from("/work/.agents/skills/real-skill/SKILL.md"),
            },
        ];
        session
    }

    #[test]
    fn test_cli_parsing_positional_prompt() {
        let args = CodingAssistant::parse_from(["cake", "test prompt"]);
        assert_eq!(args.prompt, Some("test prompt".to_string()));
    }

    #[test]
    fn test_cli_parsing_dash_for_stdin() {
        let args = CodingAssistant::parse_from(["cake", "-"]);
        assert_eq!(args.prompt, Some("-".to_string()));
    }

    #[test]
    fn test_cli_parsing_no_prompt() {
        let args = CodingAssistant::parse_from(["cake"]);
        assert_eq!(args.prompt, None);
    }

    #[test]
    fn test_cli_parsing_model_flag() {
        let args = CodingAssistant::parse_from(["cake", "--model", "claude", "test prompt"]);
        assert_eq!(args.model, Some("claude".to_string()));
        assert_eq!(args.prompt, Some("test prompt".to_string()));
    }

    #[test]
    fn test_cli_parsing_model_flag_without_prompt() {
        let args = CodingAssistant::parse_from(["cake", "--model", "deepseek"]);
        assert_eq!(args.model, Some("deepseek".to_string()));
        assert_eq!(args.prompt, None);
    }

    #[test]
    fn test_cli_parsing_no_model_flag() {
        let args = CodingAssistant::parse_from(["cake", "test prompt"]);
        assert_eq!(args.model, None);
    }

    #[test]
    fn test_cli_parsing_reasoning_effort() {
        let args =
            CodingAssistant::parse_from(["cake", "--reasoning-effort", "xhigh", "test prompt"]);
        assert_eq!(args.reasoning_effort, Some(ReasoningEffort::Xhigh));
    }

    #[test]
    fn test_cli_rejects_invalid_reasoning_effort() {
        let result =
            CodingAssistant::try_parse_from(["cake", "--reasoning-effort", "maximum", "test"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_parsing_profile_flag() {
        let args = CodingAssistant::parse_from(["cake", "--profile", "review", "test prompt"]);
        assert_eq!(args.profile, Some("review".to_string()));
        assert_eq!(args.prompt, Some("test prompt".to_string()));
    }

    #[test]
    fn test_cli_parsing_no_session() {
        let args = CodingAssistant::parse_from(["cake", "--no-session", "test prompt"]);
        assert!(args.no_session);
    }

    #[test]
    fn test_cli_parsing_no_session_defaults_false() {
        let args = CodingAssistant::parse_from(["cake", "test prompt"]);
        assert!(!args.no_session);
    }

    #[test]
    fn test_run_mode_defaults_to_new_session() {
        let args = CodingAssistant::parse_from(["cake", "test prompt"]);
        assert_eq!(RunMode::from_cli(&args).unwrap(), RunMode::NewSession);
    }

    #[test]
    fn test_run_mode_no_session_is_ephemeral() {
        let args = CodingAssistant::parse_from(["cake", "--no-session", "test prompt"]);
        assert_eq!(RunMode::from_cli(&args).unwrap(), RunMode::Ephemeral);
        assert!(!RunMode::from_cli(&args).unwrap().persists_session());
    }

    #[test]
    fn test_run_mode_restore_flags() {
        let resume_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let args = CodingAssistant::parse_from(["cake", "--continue", "test prompt"]);
        assert_eq!(RunMode::from_cli(&args).unwrap(), RunMode::ContinueLatest);

        let args = CodingAssistant::parse_from([
            "cake",
            "--resume",
            "550e8400-e29b-41d4-a716-446655440000",
            "test prompt",
        ]);
        assert_eq!(
            RunMode::from_cli(&args).unwrap(),
            RunMode::Resume {
                session_id: resume_id
            }
        );

        let args = CodingAssistant::parse_from(["cake", "--fork"]);
        assert_eq!(RunMode::from_cli(&args).unwrap(), RunMode::ForkLatest);

        let args = CodingAssistant::parse_from([
            "cake",
            "--fork",
            "550e8400-e29b-41d4-a716-446655440000",
            "test prompt",
        ]);
        assert_eq!(
            RunMode::from_cli(&args).unwrap(),
            RunMode::Fork {
                session_id: resume_id
            }
        );
    }

    #[test]
    fn test_run_mode_rejects_non_uuid_session_references() {
        let args = CodingAssistant::parse_from(["cake", "--resume", "not-a-uuid", "test prompt"]);
        assert!(
            RunMode::from_cli(&args)
                .unwrap_err()
                .to_string()
                .contains("Invalid session UUID")
        );

        let args = CodingAssistant::parse_from(["cake", "--fork", "not-a-uuid", "test prompt"]);
        assert!(
            RunMode::from_cli(&args)
                .unwrap_err()
                .to_string()
                .contains("Invalid session UUID")
        );
    }

    #[test]
    fn test_cli_parsing_add_dir_single() {
        let args =
            CodingAssistant::parse_from(["cake", "--add-dir", "/path/to/dir", "test prompt"]);
        assert_eq!(args.add_dir, vec!["/path/to/dir"]);
        assert_eq!(args.prompt, Some("test prompt".to_string()));
    }

    #[test]
    fn test_cli_parsing_add_dir_multiple() {
        let args = CodingAssistant::parse_from([
            "cake",
            "--add-dir",
            "/path/to/dir1",
            "--add-dir",
            "/path/to/dir2",
            "test prompt",
        ]);
        assert_eq!(args.add_dir, vec!["/path/to/dir1", "/path/to/dir2"]);
    }

    #[test]
    fn test_cli_parsing_add_dir_none() {
        let args = CodingAssistant::parse_from(["cake", "test prompt"]);
        assert!(args.add_dir.is_empty());
    }

    #[test]
    fn test_cli_parsing_no_skills() {
        let args = CodingAssistant::parse_from(["cake", "--no-skills", "test prompt"]);
        assert!(args.no_skills);
        assert!(args.skills.is_none());
    }

    #[test]
    fn test_cli_parsing_skills_filter() {
        let args =
            CodingAssistant::parse_from(["cake", "--skills", "debugging,review", "test prompt"]);
        assert!(!args.no_skills);
        assert_eq!(args.skills, Some("debugging,review".to_string()));
    }

    #[test]
    fn test_cli_parsing_skills_defaults() {
        let args = CodingAssistant::parse_from(["cake", "test prompt"]);
        assert!(!args.no_skills);
        assert!(args.skills.is_none());
    }

    #[test]
    fn test_resolve_model_config_no_model_configured() {
        let args = CodingAssistant::parse_from(["cake", "test prompt"]);
        let models = HashMap::new();
        let result = args.resolve_model_config(&models, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No model specified"));
        assert!(err.contains("settings.toml"));
    }

    #[test]
    fn test_resolve_model_config_default_model() {
        let args = CodingAssistant::parse_from(["cake", "test prompt"]);
        let mut models = HashMap::new();
        models.insert(
            "zen".to_string(),
            ModelDefinition {
                name: "zen".to_string(),
                model: "glm-5.1".to_string(),
                base_url: "https://opencode.ai/zen/go/v1/".to_string(),
                api_key_env: "OPENCODE_ZEN_API_TOKEN".to_string(),
                api_type: ApiType::ChatCompletions,
                temperature: None,
                top_p: None,
                max_output_tokens: None,
                reasoning_effort: None,
                reasoning_summary: None,
                reasoning_max_tokens: None,
                providers: vec![],
            },
        );

        let config = args.resolve_model_config(&models, Some("zen")).unwrap();
        assert_eq!(config.model, "glm-5.1");
    }

    #[test]
    fn test_resolve_model_config_unknown_model() {
        let mut args = CodingAssistant::parse_from(["cake", "test prompt"]);
        args.model = Some("nonexistent".to_string());

        let models = HashMap::new();
        let result = args.resolve_model_config(&models, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Unknown model 'nonexistent'"));
    }

    #[test]
    fn test_resolve_model_config_invalid_name_format() {
        let mut args = CodingAssistant::parse_from(["cake", "test prompt"]);
        args.model = Some("Invalid Name!".to_string());

        let models = HashMap::new();
        let result = args.resolve_model_config(&models, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Invalid model name 'Invalid Name!'"));
    }

    #[test]
    fn test_resolve_model_config_from_settings() {
        let args = CodingAssistant::parse_from(["cake", "--model", "claude", "test"]);

        let mut models = HashMap::new();
        models.insert(
            "claude".to_string(),
            ModelDefinition {
                name: "claude".to_string(),
                model: "anthropic/claude-3-sonnet".to_string(),
                base_url: "https://openrouter.ai/api/v1/".to_string(),
                api_key_env: "OPENROUTER_API_KEY".to_string(),
                api_type: ApiType::Responses,
                temperature: Some(0.7),
                top_p: Some(0.9),
                max_output_tokens: Some(8000),
                reasoning_effort: None,
                reasoning_summary: None,
                reasoning_max_tokens: None,
                providers: vec![],
            },
        );

        let config = args.resolve_model_config(&models, None).unwrap();
        assert_eq!(config.model, "anthropic/claude-3-sonnet");
        assert_eq!(config.api_type, ApiType::Responses);
        assert_eq!(config.temperature, Some(0.7));
        assert_eq!(config.top_p, Some(0.9));
    }

    #[test]
    fn test_resolve_model_config_model_flag_overrides_default_model() {
        let args = CodingAssistant::parse_from(["cake", "--model", "claude", "test"]);

        let mut models = HashMap::new();
        models.insert(
            "zen".to_string(),
            ModelDefinition {
                name: "zen".to_string(),
                model: "glm-5.1".to_string(),
                base_url: "https://example.com".to_string(),
                api_key_env: "KEY".to_string(),
                api_type: ApiType::ChatCompletions,
                temperature: None,
                top_p: None,
                max_output_tokens: None,
                reasoning_effort: None,
                reasoning_summary: None,
                reasoning_max_tokens: None,
                providers: vec![],
            },
        );
        models.insert(
            "claude".to_string(),
            ModelDefinition {
                name: "claude".to_string(),
                model: "anthropic/claude-3-sonnet".to_string(),
                base_url: "https://openrouter.ai/api/v1/".to_string(),
                api_key_env: "OPENROUTER_API_KEY".to_string(),
                api_type: ApiType::Responses,
                temperature: None,
                top_p: None,
                max_output_tokens: None,
                reasoning_effort: None,
                reasoning_summary: None,
                reasoning_max_tokens: None,
                providers: vec![],
            },
        );

        let config = args.resolve_model_config(&models, Some("zen")).unwrap();
        assert_eq!(config.model, "anthropic/claude-3-sonnet");
    }

    #[test]
    fn test_build_content_prompt_only() {
        let result = CodingAssistant::build_content(Some("hello"), None);
        assert_eq!(result.unwrap(), "hello");
    }

    #[test]
    fn test_build_content_stdin_only() {
        let result = CodingAssistant::build_content(None, Some("stdin content".to_string()));
        assert_eq!(result.unwrap(), "stdin content");
    }

    #[test]
    fn test_build_content_dash_with_stdin() {
        let result = CodingAssistant::build_content(Some("-"), Some("stdin content".to_string()));
        assert_eq!(result.unwrap(), "stdin content");
    }

    #[test]
    fn test_build_content_dash_without_stdin() {
        let result = CodingAssistant::build_content(Some("-"), None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No input provided via stdin")
        );
    }

    #[test]
    fn test_build_content_prompt_and_stdin() {
        let result =
            CodingAssistant::build_content(Some("instructions"), Some("file content".to_string()));
        assert_eq!(
            result.unwrap(),
            "User request:\ninstructions\n\nStdin:\nfile content"
        );
    }

    #[test]
    fn restored_session_seeds_skills_from_structured_records_only() {
        let run_session = CodingAssistant::restored_client_and_session(
            session_with_skill_records(),
            test_resolved_model_config(),
            &[(Role::System, "system".to_string())],
            &HashMap::new(),
            Arc::new(ToolContext::from_current_process()),
            uuid::uuid!("550e8400-e29b-41d4-a716-446655440001"),
        )
        .unwrap();

        let activated = run_session.agent.test_active_skills();
        assert!(activated.contains("real-skill"));
        assert!(!activated.contains("fake-skill"));
    }

    #[test]
    fn forked_session_seeds_skills_from_structured_records() {
        let restored = session_with_skill_records();
        let run_session = CodingAssistant::forked_client_and_session(
            &restored,
            test_resolved_model_config(),
            PathBuf::from("/work"),
            &[(Role::System, "system".to_string())],
            HashMap::new(),
            Arc::new(ToolContext::from_current_process()),
            uuid::uuid!("550e8400-e29b-41d4-a716-446655440001"),
        )
        .unwrap();

        assert!(
            run_session
                .agent
                .test_active_skills()
                .contains("real-skill")
        );
        assert!(matches!(run_session.storage, SessionStorage::New));
        let seed_records = run_session
            .seed_records
            .as_ref()
            .expect("fork should produce seed records");
        assert!(seed_records.iter().any(|record| matches!(
            record,
            crate::clients::SessionRecord::SkillActivated { name, .. }
                if name == "real-skill"
        )));
    }

    #[test]
    fn test_build_content_no_input() {
        let result = CodingAssistant::build_content(None, None);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("No input provided"));
        assert!(err_msg.contains("cake -"));
    }

    #[test]
    fn test_build_content_empty_prompt() {
        let result = CodingAssistant::build_content(Some(""), None);
        assert_eq!(result.unwrap(), "");
    }

    #[test]
    fn test_build_content_empty_stdin() {
        let result = CodingAssistant::build_content(None, Some(String::new()));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No input provided")
        );
    }

    #[test]
    fn test_build_content_prompt_with_empty_stdin() {
        let result = CodingAssistant::build_content(Some("my prompt"), Some(String::new()));
        assert_eq!(result.unwrap(), "my prompt");
    }

    #[test]
    fn test_build_content_multiline_prompt() {
        let result = CodingAssistant::build_content(Some("line 1\nline 2"), None);
        assert_eq!(result.unwrap(), "line 1\nline 2");
    }

    #[test]
    fn test_build_content_multiline_stdin() {
        let result =
            CodingAssistant::build_content(None, Some("stdin line 1\nstdin line 2".to_string()));
        assert_eq!(result.unwrap(), "stdin line 1\nstdin line 2");
    }

    #[test]
    fn test_build_content_multiline_both() {
        let result = CodingAssistant::build_content(
            Some("prompt line 1\nprompt line 2"),
            Some("stdin line 1\nstdin line 2".to_string()),
        );
        assert_eq!(
            result.unwrap(),
            "User request:\nprompt line 1\nprompt line 2\n\nStdin:\nstdin line 1\nstdin line 2"
        );
    }

    // Tests for format_spinner_message
    #[test]
    fn test_format_spinner_message_function_call() {
        let item = ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "Bash".to_string(),
            arguments: r#"{"command":"ls -la"}"#.to_string(),
            timestamp: None,
        };
        let msg = format_spinner_message(&item);
        assert!(msg.is_some());
        let msg = msg.unwrap_or_default();
        assert!(msg.contains("Bash:"));
        assert!(msg.contains("ls -la"));
    }

    #[test]
    fn test_format_spinner_message_reasoning() {
        let item = ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: vec!["thinking...".to_string()],
            encrypted_content: None,
            content: None,
            timestamp: None,
        };
        let msg = format_spinner_message(&item);
        assert_eq!(msg, Some("Thinking...".to_string()));
    }

    #[test]
    fn test_format_spinner_message_assistant() {
        let item = ConversationItem::Message {
            role: Role::Assistant,
            content: "Here is the answer".to_string(),
            id: Some("msg-1".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
        };
        let msg = format_spinner_message(&item);
        assert_eq!(msg, Some("Responding...".to_string()));
    }

    #[test]
    fn test_format_spinner_message_user_returns_none() {
        let item = ConversationItem::Message {
            role: Role::User,
            content: "Hello".to_string(),
            id: None,
            status: None,
            timestamp: None,
        };
        assert!(format_spinner_message(&item).is_none());
    }

    #[test]
    fn test_format_spinner_message_function_output_returns_none() {
        let item = ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "result".to_string(),
            timestamp: None,
        };
        assert!(format_spinner_message(&item).is_none());
    }

    #[test]
    fn test_format_retry_message_http_retry() {
        let status = crate::clients::retry::RetryStatus {
            attempt: 2,
            max_retries: 5,
            delay: Duration::from_millis(1_250),
            reason: crate::clients::retry::RetryReason::RateLimit,
            detail: "429 rate limit".to_string(),
        };

        assert_eq!(
            format_retry_message(&status),
            "Retrying in 1.3s after 429 rate limit (attempt 2/5)"
        );
    }

    #[test]
    fn test_format_retry_message_context_overflow() {
        let status = crate::clients::retry::RetryStatus {
            attempt: 2,
            max_retries: 5,
            delay: Duration::ZERO,
            reason: crate::clients::retry::RetryReason::ContextOverflow,
            detail: "max_output_tokens=3584".to_string(),
        };

        assert_eq!(
            format_retry_message(&status),
            "Retrying once with max_output_tokens=3584 after context overflow"
        );
    }

    #[test]
    fn test_format_done_summary() {
        temp_env::with_var("CAKE_TEST_VALID_KEY", Some("sk-test-123"), || {
            let config = ModelConfig {
                model: "test/model".to_string(),
                api_type: ApiType::ChatCompletions,
                base_url: "https://api.example.com".to_string(),
                api_key_env: "CAKE_TEST_VALID_KEY".to_string(),
                temperature: None,
                top_p: None,
                max_output_tokens: None,
                reasoning_effort: None,
                reasoning_summary: None,
                reasoning_max_tokens: None,
                providers: vec![],
            };
            let resolved = match ResolvedModelConfig::resolve(config) {
                Ok(resolved) => resolved,
                Err(err) => panic!("test config should resolve: {err}"),
            };
            let agent = Agent::new(
                resolved,
                &[(Role::System, "test system prompt".to_string())],
            )
            .with_session_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"))
            .with_turn_count(3)
            .with_total_usage(crate::clients::types::Usage {
                input_tokens: 1000,
                input_tokens_details: crate::clients::types::InputTokensDetails {
                    cached_tokens: 250,
                },
                output_tokens: 500,
                ..Default::default()
            });

            let summary = format_done_summary(1500, &agent);
            assert!(summary.contains("session 550e8400-e29b-41d4-a716-446655440000"));
            assert!(summary.contains("1.5s"));
            assert!(summary.contains("3 turns"));
            assert!(summary.contains("1000 input tokens"));
            assert!(summary.contains("250 cached reads"));
            assert!(summary.contains("500 output tokens"));
        });
    }

    #[test]
    fn output_sink_builds_success_json() {
        temp_env::with_var("CAKE_TEST_VALID_KEY", Some("sk-test-123"), || {
            let agent = Agent::new(
                test_resolved_model_config(),
                &[(Role::System, "test system prompt".to_string())],
            )
            .with_session_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"))
            .with_turn_count(2)
            .with_total_usage(crate::clients::types::Usage {
                input_tokens: 12,
                output_tokens: 8,
                ..Default::default()
            });
            let session = Session::new(agent.session_id(), PathBuf::from("/work"));
            let dir = match tempfile::tempdir() {
                Ok(dir) => dir,
                Err(err) => panic!("temp dir should be created: {err}"),
            };
            let data_dir = match temp_env::with_var("CAKE_DATA_DIR", Some(dir.path()), DataDir::new)
            {
                Ok(data_dir) => data_dir,
                Err(err) => panic!("data dir should be created: {err}"),
            };
            let result = Ok(Some(Message {
                role: Role::Assistant,
                content: "done".to_string(),
            }));

            let json = CliOutputSink::turn_result_json(
                &result,
                1500,
                &agent,
                Path::new("/work"),
                &data_dir,
                &session,
            );

            assert_eq!(json["result"], "done");
            assert_eq!(json["session_id"], agent.session_id().to_string());
            assert_eq!(json["turns"], 2);
            assert_eq!(json["elapsed_time"], 1500);
            assert_eq!(json["usage"]["input_tokens"], 12);
            assert!(json.get("error").is_none());
        });
    }

    #[test]
    fn output_sink_builds_error_json() {
        temp_env::with_var("CAKE_TEST_VALID_KEY", Some("sk-test-123"), || {
            let agent = Agent::new(
                test_resolved_model_config(),
                &[(Role::System, "test system prompt".to_string())],
            )
            .with_session_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"));
            let session = Session::new(agent.session_id(), PathBuf::from("/work"));
            let dir = match tempfile::tempdir() {
                Ok(dir) => dir,
                Err(err) => panic!("temp dir should be created: {err}"),
            };
            let data_dir = match temp_env::with_var("CAKE_DATA_DIR", Some(dir.path()), DataDir::new)
            {
                Ok(data_dir) => data_dir,
                Err(err) => panic!("data dir should be created: {err}"),
            };
            let result = Err(anyhow::anyhow!("provider failed"));

            let json = CliOutputSink::turn_result_json(
                &result,
                250,
                &agent,
                Path::new("/work"),
                &data_dir,
                &session,
            );

            assert_eq!(json["result"], serde_json::Value::Null);
            assert_eq!(json["error"], "provider failed");
            assert_eq!(json["elapsed_time"], 250);
        });
    }

    #[test]
    fn test_resolve_model_for_session_by_model_field() {
        // Session stores the API model identifier, not the config name.
        // This test verifies that --continue works when the session model
        // matches a definition's `model` field even if the `name` differs.
        temp_env::with_var("CAKE_TEST_VALID_KEY", Some("sk-test-123"), || {
            let args = CodingAssistant::parse_from(["cake", "test prompt"]);

            let mut models = HashMap::new();
            models.insert(
                "my-alias".to_string(),
                ModelDefinition {
                    name: "my-alias".to_string(),
                    model: "deepseek-v4-pro".to_string(),
                    base_url: "https://api.example.com".to_string(),
                    api_key_env: "CAKE_TEST_VALID_KEY".to_string(),
                    api_type: ApiType::ChatCompletions,
                    temperature: None,
                    top_p: None,
                    max_output_tokens: None,
                    reasoning_effort: None,
                    reasoning_summary: None,
                    reasoning_max_tokens: None,
                    providers: vec![],
                },
            );

            let resolved = args
                .resolve_model_for_session(&models, None, Some("deepseek-v4-pro"))
                .unwrap();
            assert_eq!(resolved.model_config.model, "deepseek-v4-pro");
        });
    }
}
