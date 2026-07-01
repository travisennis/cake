//! cake - AI coding assistant CLI

mod cli;
mod clients;
mod config;
mod exit_code;
mod hooks;
mod logger;
mod prompts;
mod session_telemetry;
mod time_format;
mod types;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::cli::{CliOutputSink, CmdRunner, Commands, RunMode, SessionStorage, TurnResult};
use crate::clients::{Agent, ToolContext};
use crate::config::settings::LoadedSettings;
use crate::config::{
    AgentsFile, DataDir, DiagnosticLevel, HookSource, HooksLoader, ModelConfig, ModelDefinition,
    ReasoningEffort, ResolvedModelConfig, Session, SettingsLoader, SkillCatalog, discover_skills,
    discover_skills_with_paths, parse_skill_path_list, worktree,
};
use crate::hooks::{HookContext, HookRunner};

use crate::session_telemetry::{SessionTelemetryRecord, SessionTelemetryWriter};

use crate::types::{SessionRecord, StreamRecord, TaskOutcome};

use clap::{ArgGroup, Parser, ValueEnum};

use serde::Serialize;
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
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(group(
    ArgGroup::new("session_mode")
        .args(["continue_session", "resume", "fork", "no_session"])
        .multiple(false)
))]
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

    #[command(subcommand)]
    pub command: Option<Commands>,
}

struct PreparedRun {
    current_dir: PathBuf,
    additional_dirs: Vec<PathBuf>,
    _worktree: WorktreeGuard,
    content: String,
}

struct RunResources {
    loaded: LoadedSettings,
    agents_files: Vec<AgentsFile>,
    skill_catalog: SkillCatalog,
    tool_context: Arc<ToolContext>,
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
                    // Canonicalize to produce a stable absolute path that
                    // remains valid even if the process cwd changes later.
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

    fn prepare_run(&self) -> anyhow::Result<PreparedRun> {
        let original_dir = std::env::current_dir()?;
        let additional_dirs = self.resolve_additional_dirs(&original_dir);

        // Validate stdin/content before creating the worktree so that
        // input errors don't leave a stale registered worktree.
        let stdin_content = Self::read_stdin_content()?;
        let content = Self::build_content(self.prompt.as_deref(), stdin_content)?;

        // Set up the worktree after input validation (if requested).
        let worktree = self.setup_worktree(&original_dir)?;
        let current_dir = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("Failed to get current directory: {e}"))?;

        Ok(PreparedRun {
            current_dir,
            additional_dirs,
            _worktree: WorktreeGuard::new(worktree, original_dir),
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
        storage: SessionStorage,
        persists_session: bool,
    ) -> anyhow::Result<(Agent, Option<crate::config::SessionWriter>)> {
        if !persists_session {
            return Ok((client, None));
        }

        let file = match storage {
            SessionStorage::New => data_dir.create_session_file(session, client.tool_names())?,
            SessionStorage::Append => data_dir.open_session_for_append(session.id)?,
        };
        let writer = crate::config::SessionWriter::new(file);
        let writer_for_callback = writer.clone();
        client =
            client.with_persist_callback(move |record| writer_for_callback.append_record(record));
        Ok((client, Some(writer)))
    }

    fn prepare_seeded_session(
        data_dir: &DataDir,
        run_session: &mut crate::cli::RunSession,
    ) -> anyhow::Result<()> {
        if let Some(seed_records) = run_session.seed_records.take() {
            let mut file = data_dir
                .create_session_file(&run_session.session, run_session.agent.tool_names())?;
            crate::config::Session::append_records(&mut file, &seed_records)?;
            run_session.storage = SessionStorage::Append;
        }

        Ok(())
    }

    fn attach_session_telemetry(
        client: Agent,
        data_dir: &DataDir,
        session: &Session,
        run_mode: &RunMode,
        current_dir: &Path,
        output_format: OutputFormat,
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
        let settings = client.session_telemetry_settings(output_format);
        let record = SessionTelemetryRecord::TelemetryInit {
            session_id: session.id.to_string(),
            invocation_id: invocation_id.to_string(),
            timestamp: chrono::Utc::now(),
            mode: run_mode.telemetry_mode(),
            working_directory: current_dir.display().to_string(),
            model: client.model_name().to_string(),
            api_type: settings.api_type,
            output_format,
            tools: client.tool_names(),
            settings,
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
        current_dir: &Path,
        hook_context: HookContext,
    ) -> anyhow::Result<(Agent, Option<Arc<HookRunner>>)> {
        let hooks = HooksLoader::load(current_dir)?;

        if hooks.is_empty() {
            return Ok((client, None));
        }

        let runner = Arc::new(HookRunner::new(hooks, hook_context));
        client = client.with_hook_runner(Arc::clone(&runner));
        Ok((client, Some(runner)))
    }

    fn hook_event_sink(
        session_writer: Option<crate::config::SessionWriter>,
        output_format: OutputFormat,
    ) -> Option<Arc<dyn Fn(StreamRecord) + Send + Sync>> {
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
                    Ok(json) => CliOutputSink::write_stream_record(&json),
                    Err(error) => tracing::warn!("Stream serialization failed: {error}"),
                }
            }
        }))
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
        let start = Instant::now();

        if let Some(runner) = hook_runner {
            let contexts = runner.session_start(&session_start_source, content).await?;
            client.append_developer_context(contexts);
            let contexts = runner.user_prompt_submit(content).await?;
            client.append_developer_context(contexts);
        }
        client.emit_prompt_context_records()?;
        client.emit_task_start_record()?;

        let result = client.send(content.to_string()).await;
        let duration_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

        Self::handle_agent_turn_result(client, hook_runner, &result, duration_ms).await?;

        Ok(TurnResult {
            result,
            duration_ms,
        })
    }

    /// Handle the result of `client.send()`: invoke stop/error hooks and emit the
    /// task completion record. Extracted from `execute_agent_turn` for focused
    /// unit-testing without requiring a real API call.
    async fn handle_agent_turn_result(
        client: &mut Agent,
        hook_runner: Option<&Arc<HookRunner>>,
        result: &Result<String, anyhow::Error>,
        duration_ms: u64,
    ) -> anyhow::Result<()> {
        match result {
            Ok(response_text) => {
                if let Some(runner) = hook_runner {
                    match runner.stop(response_text).await {
                        Ok(Some(context)) => {
                            tracing::info!(target: "cake::hooks", additional_context = %context, "Stop hook returned additional context");
                        },
                        Ok(None) => {},
                        Err(error) => {
                            tracing::warn!(target: "cake::hooks", error = %error, "Stop hook failed (best-effort)");
                        },
                    }
                }
                client.emit_task_complete_record(
                    TaskOutcome::Success {
                        result: Some(response_text.clone()),
                    },
                    duration_ms,
                )?;
            },
            Err(e) => {
                if let Some(runner) = hook_runner
                    && let Err(error) = runner.error_occurred(e).await
                {
                    tracing::warn!(target: "cake::hooks", error = %error, "error_occurred hook failed (best-effort)");
                }
                client.emit_task_complete_record(
                    TaskOutcome::ErrorDuringExecution {
                        error: e.to_string(),
                    },
                    duration_ms,
                )?;
            },
        }
        Ok(())
    }
}

impl CmdRunner for CodingAssistant {
    async fn run(&self, data_dir: &DataDir) -> anyhow::Result<()> {
        if let Some(command) = &self.command {
            return command.run(data_dir).await;
        }

        let prepared = self.prepare_run()?;
        let resources =
            self.load_run_resources(data_dir, &prepared.current_dir, prepared.additional_dirs)?;
        let task_id = uuid::Uuid::new_v4();
        let run_mode = RunMode::from_cli(self)?;
        let config_dir = crate::config::config_dir().join("cake");
        let mut run_session = self.build_client_and_session(
            &run_mode,
            data_dir,
            prepared.current_dir.clone(),
            &config_dir,
            &resources.agents_files,
            &resources.loaded.models,
            resources.loaded.default_model.as_deref(),
            &resources.skill_catalog,
            &resources.tool_context,
            task_id,
        )?;

        Self::prepare_seeded_session(data_dir, &mut run_session)?;

        let session_start_source =
            HookSource::SessionStart(run_mode.session_start_source().to_owned());
        let session = run_session.session;
        let (client, session_writer) = Self::attach_persistence(
            run_session.agent,
            data_dir,
            &session,
            run_session.storage,
            run_mode.persists_session(),
        )?;
        let client = Self::attach_session_telemetry(
            client,
            data_dir,
            &session,
            &run_mode,
            &prepared.current_dir,
            self.output_format,
        );
        let hook_context = HookContext {
            session_id: session.id,
            task_id,
            transcript_path: run_mode
                .persists_session()
                .then(|| data_dir.session_path(session.id)),
            session_writer: session_writer.clone(),
            hook_event_sink: Self::hook_event_sink(session_writer, self.output_format),
            cwd: prepared.current_dir.clone(),
            model: client.model_name().to_string(),
        };
        let (client, hook_runner) =
            Self::attach_hooks(client, &prepared.current_dir, hook_context)?;
        let output = self.output_sink();
        let mut client = output.attach_callbacks(client);

        // Race Ctrl-C against the agent turn so we can emit a clean
        // TaskComplete record even when interrupted.
        let interrupted = AtomicBool::new(false);
        let turn_start = Instant::now();

        let turn: TurnResult = tokio::select! {
            biased;
            result = async {
                Self::execute_agent_turn(
                    &mut client,
                    hook_runner.as_ref(),
                    session_start_source,
                    &prepared.content,
                )
                .await
            } => match result {
                Ok(t) => t,
                Err(e) => return Err(e),
            },
            _ = tokio::signal::ctrl_c() => {
                interrupted.store(true, Ordering::SeqCst);
                // Dummy value — the flag check below short-circuits
                // before `turn` is used when interrupted is true.
                TurnResult {
                    result: Ok(String::new()),
                    duration_ms: 0,
                }
            },
        };

        if interrupted.load(Ordering::SeqCst) {
            return Self::handle_interrupt(&mut client, turn_start);
        }

        // The agent turn completed normally.
        client.emit_session_summary_telemetry(
            turn.result.is_ok(),
            turn.duration_ms,
            turn.result.as_ref().err().map(ToString::to_string),
        );
        output.render_turn(
            turn,
            &client,
            &prepared.current_dir,
            data_dir,
            &session,
            run_mode.persists_session(),
        )?;

        // Worktree cleanup is handled by WorktreeGuard's Drop.

        Ok(())
    }
}

impl CodingAssistant {
    /// Handle a user interrupt (Ctrl-C) during an agent turn.
    ///
    /// Emits a `TaskComplete` record with an `Interrupted` outcome, writes
    /// the telemetry summary, and returns an `Interrupted` error that
    /// `main()` maps to exit code 130. Worktree cleanup is handled by
    /// [`WorktreeGuard`]'s `Drop`.
    fn handle_interrupt(client: &mut Agent, turn_start: Instant) -> anyhow::Result<()> {
        // Set up a second Ctrl-C handler that force-exits immediately
        // in case the graceful shutdown hangs.
        let second_ctrlc = tokio::spawn(async {
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(exit_code::code::INTERRUPTED.into());
            }
        });

        let elapsed: u64 = turn_start
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);

        // Emit a TaskComplete record with interrupted outcome so the
        // session file always has a matching end for the TaskStart.
        if let Err(e) = client.emit_task_complete_record(TaskOutcome::Interrupted, elapsed) {
            tracing::warn!(
                target: "cake",
                "Failed to emit interrupted TaskComplete record: {e}"
            );
        }

        // Write the telemetry summary with success=false.
        client.emit_session_summary_telemetry(
            false,
            elapsed,
            Some("Interrupted by user".to_string()),
        );

        // Abort the second-Ctrl-C listener. Worktree cleanup runs
        // later via WorktreeGuard's Drop when `prepared` goes out of
        // scope.
        second_ctrlc.abort();

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

        // Restore the original working directory first.
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
                tracing::warn!(
                    "Could not check worktree '{}' for changes, keeping it: {e}",
                    wt.name
                );
            },
        }
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args = match CodingAssistant::try_parse() {
        Ok(a) => a,
        Err(e) => {
            // Print the clap error (includes --help and --version output).
            // For --help/--version, clap returns exit_code() == 0 and the
            // formatted output goes to stdout. For actual errors (bad flags,
            // missing required args), it goes to stderr with exit_code() != 0.
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

    match args.run(&data_dir).await {
        Ok(()) => std::process::ExitCode::from(exit_code::code::SUCCESS),
        Err(e) => {
            if e.is::<Interrupted>() {
                return std::process::ExitCode::from(exit_code::code::INTERRUPTED);
            }
            CliOutputSink::write_error(&e);
            exit_code::classify(&e)
        },
    }
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
