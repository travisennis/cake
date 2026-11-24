//! Session construction from CLI arguments.
//!
//! Provides the free function [`skill_locations`] and
//! `impl CodingAssistant` methods that build agent/session pairs
//! for new, restored, and forked runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::info;

use crate::cli::context_identity;
use crate::cli::run_mode::{RunMode, SessionPersistencePlan};
use crate::clients::mcp::McpServer;
use crate::clients::{Agent, ToolContext};
use crate::config::provider_identity::{self, SessionProviderIdentityCheck};
use crate::config::settings::{ResolvedImagePolicy, SkillSettings};
use crate::config::skills::Skill;
use crate::config::toolbox::ToolboxTool;
use crate::config::{
    AgentsFile, DataDir, ModelDefinition, ResolvedModelConfig, Session, SkillCatalog,
    SkillCatalogBudget,
};
use crate::prompts::build_initial_prompt_messages_with_mcp;
use crate::types::{
    ContextIdentity, PromptPrefixDecision, ProviderIdentity, Role, SessionRecord,
    StoredPromptPrefix, prompt_prefix_hash,
};

/// A fully assembled agent, session, and persistence plan ready for execution.
pub struct RunSession {
    pub(crate) agent: Agent,
    pub(crate) session: Session,
    pub(crate) persistence: Option<SessionPersistencePlan>,
    /// Sanitized identity resolved for this invocation, including a legacy
    /// session whose stored snapshot is absent.
    pub(crate) effective_provider_identity: Option<ProviderIdentity>,
    /// Whether a resume or fork accepted a changed provider identity for this
    /// invocation only.
    pub(crate) session_model_change_override: bool,
    /// The latest complete prefix stored in the restored session, if any.
    pub(crate) stored_prompt_prefix: Option<StoredPromptPrefix>,
    /// Whether the current prompt includes the runtime-context message.
    pub(crate) include_runtime_context: bool,
    /// Whether this run created, reused, or rebuilt the leading prefix.
    pub(crate) prompt_prefix_decision: PromptPrefixDecision,
    /// Conversation model configuration after restore and CLI overrides. Used
    /// to derive a separate compaction summarizer configuration. Boxed to keep
    /// the run future small.
    pub(crate) resolved: Box<ResolvedModelConfig>,
}

impl RunSession {
    /// Attach a compiled `--output-schema` to the agent, if one was given.
    pub(crate) fn attach_output_schema(
        &mut self,
        schema: Option<&Arc<crate::config::OutputSchema>>,
    ) {
        if let Some(schema) = schema {
            self.agent.set_output_schema(Arc::clone(schema));
        }
    }

    /// Attach the run's `--tool-choice`, if one was given.
    pub(crate) const fn attach_tool_choice(&mut self, choice: Option<crate::clients::ToolChoice>) {
        if let Some(choice) = choice {
            self.agent.set_tool_choice(choice);
        }
    }

    /// Reuse the stored leading prompt prefix when every gate passes.
    ///
    /// Reuse requires an exact full context-identity match, a known and
    /// unchanged provider identity, identical stable bytes, and the same
    /// runtime-message presence. The runtime message itself is always rebuilt
    /// from the current process environment.
    ///
    /// # Errors
    ///
    /// Returns an error when the replacement prefix cannot be applied to the
    /// restored conversation.
    pub(crate) fn apply_prompt_prefix_reuse(
        &mut self,
        refresh_context: bool,
        current_identity: &ContextIdentity,
        provider_identity_unchanged: bool,
    ) -> anyhow::Result<bool> {
        let reused = self.try_reuse_prompt_prefix(
            refresh_context,
            current_identity,
            provider_identity_unchanged,
        )?;
        let decision = if reused {
            PromptPrefixDecision::Reused
        } else {
            self.prompt_prefix_decision
        };
        self.prompt_prefix_decision = decision;
        self.agent.set_prompt_prefix_metadata(
            context_identity::fingerprint(current_identity),
            decision,
            self.include_runtime_context,
        );
        Ok(reused)
    }

    fn try_reuse_prompt_prefix(
        &mut self,
        refresh_context: bool,
        current_identity: &ContextIdentity,
        provider_identity_unchanged: bool,
    ) -> anyhow::Result<bool> {
        if refresh_context || !provider_identity_unchanged {
            return Ok(false);
        }
        let Some(stored) = self.stored_prompt_prefix.as_ref() else {
            return Ok(false);
        };
        if stored.context_fingerprint != context_identity::fingerprint(current_identity) {
            return Ok(false);
        }
        let Some(prefix) = self.reusable_prompt_prefix(stored) else {
            return Ok(false);
        };
        let system_prompt = stored.system_prompt.clone();
        self.agent.replace_leading_prompt(&prefix)?;
        self.session.system_prompt = Some(system_prompt);
        self.prompt_prefix_decision = PromptPrefixDecision::Reused;
        Ok(true)
    }

    fn reusable_prompt_prefix(&self, stored: &StoredPromptPrefix) -> Option<Vec<(Role, String)>> {
        let (system_prompt, developer, runtime_context) = self.current_prompt_parts()?;
        if !prefix_matches(stored, &system_prompt, &developer, runtime_context) {
            return None;
        }
        Some(build_reused_prefix(stored, &developer, runtime_context))
    }

    fn current_prompt_parts(&self) -> Option<(String, Vec<String>, bool)> {
        let messages = self.agent.leading_prompt_messages();
        let (role, system_prompt) = messages.first()?;
        if *role != Role::System {
            return None;
        }
        let developer: Vec<String> = messages
            .iter()
            .filter(|(role, _)| matches!(role, Role::Developer))
            .map(|(_, content)| content.clone())
            .collect();
        let runtime_context = self.include_runtime_context && !developer.is_empty();
        Some((system_prompt.clone(), developer, runtime_context))
    }
}

const fn stable_developer_len(runtime_context: bool, count: usize) -> usize {
    if runtime_context {
        count.saturating_sub(1)
    } else {
        count
    }
}

fn prefix_matches(
    stored: &StoredPromptPrefix,
    system_prompt: &str,
    developer: &[String],
    runtime_context: bool,
) -> bool {
    if stored.system_prompt != system_prompt {
        return false;
    }
    if stored.developer_messages.len() != developer.len() {
        return false;
    }
    if stored.runtime_context != runtime_context {
        return false;
    }
    let current_len = stable_developer_len(runtime_context, developer.len());
    let stored_len = stable_developer_len(stored.runtime_context, stored.developer_messages.len());
    if stored.developer_messages[..stored_len] != developer[..current_len] {
        return false;
    }
    stored.stable_prompt_hash == prompt_prefix_hash(system_prompt, &developer[..current_len])
}

fn build_reused_prefix(
    stored: &StoredPromptPrefix,
    developer: &[String],
    runtime_context: bool,
) -> Vec<(Role, String)> {
    let mut prefix = Vec::with_capacity(1 + stored.developer_messages.len());
    prefix.push((Role::System, stored.system_prompt.clone()));
    prefix.extend(
        stored
            .developer_messages
            .iter()
            .cloned()
            .map(|content| (Role::Developer, content)),
    );
    if runtime_context
        && let Some(runtime) = developer.last()
        && let Some(last) = prefix.last_mut()
    {
        last.1.clone_from(runtime);
    }
    prefix
}

/// Build a map of skill file paths to skills for activation deduplication.
pub fn skill_locations(skill_catalog: &SkillCatalog) -> HashMap<PathBuf, Skill> {
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

/// Warn once when a restored or forked history carries images the resolved
/// model cannot accept.
///
/// The images stay in the stored session; request builders omit them. Emitting
/// the warning here keeps it to one line per run instead of one per request.
fn warn_omitted_history_images(
    agent: &Agent,
    resolved: &ResolvedModelConfig,
    model_config_name: &str,
) {
    if let Some(warning) = omitted_history_images_warning(
        resolved.model_config.supports_images,
        agent.history_image_count(),
        &resolved.model_config.model,
        model_config_name,
    ) {
        eprintln!("warning: {warning}");
    }
}

fn omitted_history_images_warning(
    supports_images: bool,
    image_count: usize,
    model: &str,
    model_config_name: &str,
) -> Option<String> {
    if supports_images || image_count == 0 {
        return None;
    }
    let noun = if image_count == 1 { "image" } else { "images" };
    Some(format!(
        "model '{model}' (config '{model_config_name}') cannot accept images; \
         omitting {image_count} {noun} from conversation history for this run"
    ))
}

impl crate::CodingAssistant {
    /// Convert a restored session into the agent/session pair used for a continued run.
    #[expect(
        clippy::too_many_arguments,
        reason = "session construction naturally requires many parameters"
    )]
    pub(crate) fn restored_client_and_session(
        restored: Session,
        resolved: ResolvedModelConfig,
        model_config_name: String,
        effective_provider_identity: ProviderIdentity,
        session_model_change_override: bool,
        initial_messages: &[(Role, String)],
        skill_locations: &HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        toolbox_tools: Vec<ToolboxTool>,
        mcp_servers: Vec<McpServer>,
        enabled_tools: Option<&[String]>,
        include_runtime_context: bool,
        task_id: uuid::Uuid,
        persistence: Option<SessionPersistencePlan>,
    ) -> anyhow::Result<RunSession> {
        let stored_prompt_prefix = restored.stored_prompt_prefix();
        let seed = restored.context_seed();
        let activated_skills = restored.activated_skills();
        let activated_mcp_tools = restored.activated_mcp_tools();

        let mut agent = Agent::new(resolved.clone(), initial_messages)
            .with_session_id(restored.id)
            .with_task_id(task_id)
            .with_model_config_name(model_config_name.clone())
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_mcp_servers(mcp_servers)
            .with_enabled_tools(enabled_tools)
            .with_active_mcp_tools(activated_mcp_tools)
            .with_context_seed(seed)
            .map_err(|error| anyhow::anyhow!("Cannot restore session {}: {error:#}", restored.id))?
            .with_last_usage(restored.last_turn_usage())
            .with_skill_locations(skill_locations.clone())
            .with_activated_skills(activated_skills);
        agent.set_prompt_prefix_runtime(include_runtime_context);
        warn_omitted_history_images(&agent, &resolved, &model_config_name);
        let mut session = Session::new(restored.id, restored.working_dir);
        session.model = Some(resolved.model_config.model.clone());
        session.model_config = Some(model_config_name);
        session.provider_identity = restored.provider_identity;
        session.context_identity = restored.context_identity;
        session.system_prompt = initial_messages.first().map(|(_, content)| content.clone());
        Ok(RunSession {
            agent,
            session,
            persistence,
            effective_provider_identity: Some(effective_provider_identity),
            session_model_change_override,
            stored_prompt_prefix,
            include_runtime_context,
            prompt_prefix_decision: PromptPrefixDecision::Rebuilt,
            resolved: Box::new(resolved),
        })
    }

    /// Build the agent/session pair for a new run using the agent-generated session id.
    #[expect(
        clippy::too_many_arguments,
        reason = "session construction naturally requires many parameters"
    )]
    pub(crate) fn new_client_and_session(
        resolved: ResolvedModelConfig,
        model_config_name: String,
        provider_identity: ProviderIdentity,
        current_dir: PathBuf,
        initial_messages: &[(Role, String)],
        skill_locations: HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        toolbox_tools: Vec<ToolboxTool>,
        mcp_servers: Vec<McpServer>,
        enabled_tools: Option<&[String]>,
        include_runtime_context: bool,
        task_id: uuid::Uuid,
        persistence: Option<SessionPersistencePlan>,
    ) -> RunSession {
        let mut agent = Agent::new(resolved.clone(), initial_messages)
            .with_task_id(task_id)
            .with_model_config_name(model_config_name.clone())
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_mcp_servers(mcp_servers)
            .with_enabled_tools(enabled_tools)
            .with_skill_locations(skill_locations);
        agent.set_prompt_prefix_runtime(include_runtime_context);
        let new_id = agent.session_id();
        info!(target: "cake", "New session: {new_id}");
        let mut session = Session::new(new_id, current_dir);
        session.model = Some(resolved.model_config.model.clone());
        session.model_config = Some(model_config_name);
        session.provider_identity = Some(provider_identity.clone());
        session.system_prompt = initial_messages.first().map(|(_, content)| content.clone());
        RunSession {
            agent,
            session,
            persistence,
            effective_provider_identity: Some(provider_identity),
            session_model_change_override: false,
            stored_prompt_prefix: None,
            include_runtime_context,
            prompt_prefix_decision: PromptPrefixDecision::Initial,
            resolved: Box::new(resolved),
        }
    }

    /// Build the agent/session pair for a forked run using a fresh agent session id.
    #[expect(
        clippy::too_many_arguments,
        reason = "session construction naturally requires many parameters"
    )]
    pub(crate) fn forked_client_and_session(
        restored: &Session,
        resolved: ResolvedModelConfig,
        model_config_name: String,
        effective_provider_identity: ProviderIdentity,
        session_model_change_override: bool,
        current_dir: PathBuf,
        initial_messages: &[(Role, String)],
        skill_locations: HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        toolbox_tools: Vec<ToolboxTool>,
        mcp_servers: Vec<McpServer>,
        enabled_tools: Option<&[String]>,
        include_runtime_context: bool,
        task_id: uuid::Uuid,
    ) -> anyhow::Result<RunSession> {
        let stored_prompt_prefix = restored.stored_prompt_prefix();
        let activated_mcp_tools = restored.activated_mcp_tools();
        let mut agent = Agent::new(resolved.clone(), initial_messages)
            .with_task_id(task_id)
            .with_model_config_name(model_config_name.clone())
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_mcp_servers(mcp_servers)
            .with_enabled_tools(enabled_tools)
            .with_active_mcp_tools(activated_mcp_tools);
        agent.set_prompt_prefix_runtime(include_runtime_context);
        let new_id = agent.session_id();
        let agent = agent
            .with_context_seed(restored.context_seed().for_session(new_id.to_string()))
            .map_err(|error| anyhow::anyhow!("Cannot fork session {}: {error:#}", restored.id))?
            .with_last_usage(restored.last_turn_usage())
            .with_skill_locations(skill_locations)
            .with_activated_skills(restored.activated_skills());
        warn_omitted_history_images(&agent, &resolved, &model_config_name);
        let latest_checkpoint = restored.latest_checkpoint_index();
        let seed_records: Vec<_> = restored
            .records
            .iter()
            .enumerate()
            .filter_map(|(index, record)| match record {
                record if record.to_conversation_item().is_some() => Some(record.clone()),
                SessionRecord::SkillActivated {
                    task_id,
                    timestamp,
                    name,
                    path,
                    ..
                } => Some(SessionRecord::SkillActivated {
                    session_id: new_id.to_string(),
                    task_id: task_id.clone(),
                    timestamp: *timestamp,
                    name: name.clone(),
                    path: path.clone(),
                }),
                SessionRecord::McpToolsActivated {
                    task_id,
                    timestamp,
                    tools,
                    ..
                } => Some(SessionRecord::McpToolsActivated {
                    session_id: new_id.to_string(),
                    task_id: task_id.clone(),
                    timestamp: *timestamp,
                    tools: tools.clone(),
                }),
                SessionRecord::Compaction(checkpoint) if latest_checkpoint == Some(index) => Some(
                    SessionRecord::Compaction(checkpoint.for_session(new_id.to_string())),
                ),
                _ => None,
            })
            .collect();
        info!(target: "cake", "New forked session: {new_id}");
        let mut session = Session::new(new_id, current_dir);
        session.model = Some(resolved.model_config.model.clone());
        session.model_config = Some(model_config_name);
        session
            .provider_identity
            .clone_from(&restored.provider_identity);
        session.system_prompt = initial_messages.first().map(|(_, content)| content.clone());
        Ok(RunSession {
            agent,
            session,
            persistence: Some(SessionPersistencePlan::Create {
                seed_records,
                blob_source: Some(restored.id),
            }),
            effective_provider_identity: Some(effective_provider_identity),
            session_model_change_override,
            stored_prompt_prefix,
            include_runtime_context,
            prompt_prefix_decision: PromptPrefixDecision::Rebuilt,
            resolved: Box::new(resolved),
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "session construction naturally requires many parameters"
    )]
    pub(crate) fn build_client_and_session(
        &self,
        run_mode: &RunMode,
        data_dir: &DataDir,
        current_dir: PathBuf,
        config_dir: &Path,
        agents_files: &[AgentsFile],
        models: &HashMap<String, ModelDefinition>,
        images: &ResolvedImagePolicy,
        default_model: Option<&str>,
        skill_catalog: &SkillCatalog,
        tool_context: &Arc<ToolContext>,
        toolbox_tools: &[ToolboxTool],
        mcp_servers: &[McpServer],
        enabled_tools: Option<&[String]>,
        task_id: uuid::Uuid,
        cli_system_prompt: Option<&Path>,
        loaded_system_prompt: Option<&str>,
        skill_settings: &SkillSettings,
        include_project_prompt: bool,
    ) -> anyhow::Result<RunSession> {
        let inputs = RunInputs {
            current_dir,
            config_dir,
            cli_system_prompt,
            settings_system_prompt: loaded_system_prompt.map(Path::new),
            agents_files,
            skill_catalog,
            skill_locations: skill_locations(skill_catalog),
            models,
            images,
            default_model,
            skill_settings,
            tool_context,
            toolbox_tools,
            mcp_servers,
            tools_enabled: enabled_tools,
            include_runtime_context: !self.no_runtime_context,
            include_project_prompt,
            task_id,
        };
        match run_mode {
            RunMode::Resume {
                session_id,
                read_only,
            } => self.resume_run(*session_id, *read_only, data_dir, &inputs),
            RunMode::ForkLatest | RunMode::Fork { .. } => {
                self.forked_run(run_mode, data_dir, &inputs)
            },
            RunMode::NewSession | RunMode::Ephemeral => self.new_run(
                &inputs,
                run_mode
                    .persists_session()
                    .then_some(SessionPersistencePlan::Create {
                        seed_records: Vec::new(),
                        blob_source: None,
                    }),
            ),
        }
    }

    /// Resolve a restored session's model, verify its pinned provider
    /// identity, and rebuild the pair.
    fn restored_run(
        &self,
        restored: Session,
        inputs: &RunInputs<'_>,
        persistence: Option<SessionPersistencePlan>,
    ) -> anyhow::Result<RunSession> {
        let (config, model_config_name) = self.resolve_model_selection(
            inputs.models,
            inputs.default_model,
            restored.model.as_deref(),
            restored.model_config.as_deref(),
        )?;
        let provider_identity = provider_identity::build(&config, &model_config_name)?;
        let session_model_change_override = verify_session_provider_identity(
            restored.provider_identity.as_ref(),
            &provider_identity,
            self.allow_session_model_change,
            restored.id,
        )?;
        let resolved = ResolvedModelConfig::resolve(self.apply_cli_overrides_to_config(config))?;
        let initial_messages = inputs.initial_messages(
            resolved.model_config.supports_images,
            resolved.model_config.context_window,
        );
        let tool_context = Arc::clone(inputs.tool_context);
        let mut run = Self::restored_client_and_session(
            restored,
            resolved,
            model_config_name,
            provider_identity,
            session_model_change_override,
            &initial_messages,
            &inputs.skill_locations,
            tool_context,
            inputs.toolbox_tools.to_vec(),
            inputs.mcp_servers.to_vec(),
            inputs.tools_enabled,
            inputs.include_runtime_context,
            inputs.task_id,
            persistence,
        )?;
        run.agent = run.agent.with_image_policy(inputs.images);
        Ok(run)
    }

    /// `--resume <UUID>`: restore the named session.
    fn resume_run(
        &self,
        session_id: uuid::Uuid,
        read_only: bool,
        data_dir: &DataDir,
        inputs: &RunInputs<'_>,
    ) -> anyhow::Result<RunSession> {
        let restored = data_dir
            .load_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session {session_id} not found"))?;
        info!(target: "cake", "Resumed session: {}", restored.id);
        let persistence = (!read_only).then_some(SessionPersistencePlan::Append);
        self.restored_run(restored, inputs, persistence)
    }

    /// `--fork`: start a fresh session seeded from a prior one.
    fn forked_run(
        &self,
        run_mode: &RunMode,
        data_dir: &DataDir,
        inputs: &RunInputs<'_>,
    ) -> anyhow::Result<RunSession> {
        info!(target: "cake", "Forking session");
        let restored = fork_source(run_mode, data_dir, &inputs.current_dir)?;
        info!(target: "cake", "Forking from session: {}", restored.id);
        let (config, model_config_name) = self.resolve_model_selection(
            inputs.models,
            inputs.default_model,
            restored.model.as_deref(),
            restored.model_config.as_deref(),
        )?;
        let provider_identity = provider_identity::build(&config, &model_config_name)?;
        let session_model_change_override = verify_session_provider_identity(
            restored.provider_identity.as_ref(),
            &provider_identity,
            self.allow_session_model_change,
            restored.id,
        )?;
        let resolved = ResolvedModelConfig::resolve(self.apply_cli_overrides_to_config(config))?;
        let initial_messages = inputs.initial_messages(
            resolved.model_config.supports_images,
            resolved.model_config.context_window,
        );
        let tool_context = Arc::clone(inputs.tool_context);
        let mut run = Self::forked_client_and_session(
            &restored,
            resolved,
            model_config_name,
            provider_identity,
            session_model_change_override,
            inputs.current_dir.clone(),
            &initial_messages,
            inputs.skill_locations.clone(),
            tool_context,
            inputs.toolbox_tools.to_vec(),
            inputs.mcp_servers.to_vec(),
            inputs.tools_enabled,
            inputs.include_runtime_context,
            inputs.task_id,
        )?;
        run.agent = run.agent.with_image_policy(inputs.images);
        Ok(run)
    }

    /// New and ephemeral runs: start fresh from the CLI-selected model.
    fn new_run(
        &self,
        inputs: &RunInputs<'_>,
        persistence: Option<SessionPersistencePlan>,
    ) -> anyhow::Result<RunSession> {
        let (config, model_config_name) =
            self.resolve_configured_model(inputs.models, inputs.default_model)?;
        let provider_identity = provider_identity::build(&config, &model_config_name)?;
        let resolved = ResolvedModelConfig::resolve(self.apply_cli_overrides_to_config(config))?;
        let initial_messages = inputs.initial_messages(
            resolved.model_config.supports_images,
            resolved.model_config.context_window,
        );
        let mut run = Self::new_client_and_session(
            resolved,
            model_config_name,
            provider_identity,
            inputs.current_dir.clone(),
            &initial_messages,
            inputs.skill_locations.clone(),
            Arc::clone(inputs.tool_context),
            inputs.toolbox_tools.to_vec(),
            inputs.mcp_servers.to_vec(),
            inputs.tools_enabled,
            inputs.include_runtime_context,
            inputs.task_id,
            persistence,
        );
        run.agent = run.agent.with_image_policy(inputs.images);
        Ok(run)
    }
}

/// Verify a restored or forked session against its pinned provider identity.
///
/// Returns whether `--allow-session-model-change` accepted a changed identity
/// for this invocation. A legacy session without a snapshot is an unknown
/// baseline: it warns once and resolves normally, preserving compatibility.
fn verify_session_provider_identity(
    stored: Option<&ProviderIdentity>,
    current: &ProviderIdentity,
    allow_change: bool,
    session_id: uuid::Uuid,
) -> anyhow::Result<bool> {
    match provider_identity::check_session_identity(stored, current) {
        SessionProviderIdentityCheck::Match => Ok(false),
        SessionProviderIdentityCheck::UnknownBaseline => {
            eprintln!(
                "warning: session {session_id} has no provider identity snapshot; \
                 skipping the provider identity check for this run"
            );
            Ok(false)
        },
        SessionProviderIdentityCheck::Changed(changes) => {
            let detail = changes
                .iter()
                .map(crate::types::provider_identity::ProviderIdentityDifference::describe)
                .collect::<Vec<_>>()
                .join("\n  ");
            if allow_change {
                eprintln!(
                    "warning: session {session_id} provider identity changed since it was created; \
                     continuing because --allow-session-model-change was given:\n  {detail}"
                );
                Ok(true)
            } else {
                Err(anyhow::anyhow!(
                    "Session model mismatch: provider identity for session {session_id} changed since it was created:\n  {detail}\n\
                     Use --allow-session-model-change to continue this session against the current configuration for one run, \
                     or start a new session."
                ))
            }
        },
    }
}

/// Construction inputs every run-mode step shares.
struct RunInputs<'a> {
    current_dir: PathBuf,
    config_dir: &'a Path,
    cli_system_prompt: Option<&'a Path>,
    settings_system_prompt: Option<&'a Path>,
    agents_files: &'a [AgentsFile],
    skill_catalog: &'a SkillCatalog,
    skill_locations: HashMap<PathBuf, Skill>,
    models: &'a HashMap<String, ModelDefinition>,
    images: &'a ResolvedImagePolicy,
    default_model: Option<&'a str>,
    skill_settings: &'a SkillSettings,
    tool_context: &'a Arc<ToolContext>,
    toolbox_tools: &'a [ToolboxTool],
    mcp_servers: &'a [McpServer],
    tools_enabled: Option<&'a [String]>,
    include_runtime_context: bool,
    include_project_prompt: bool,
    task_id: uuid::Uuid,
}

impl RunInputs<'_> {
    /// Build the initial prompt messages for a resolved model.
    ///
    /// The tool list is built after the model is resolved so it can reflect
    /// model capabilities: a model that does not accept image input never
    /// advertises `ReadImage`.
    fn initial_messages(
        &self,
        supports_images: bool,
        context_window: Option<u32>,
    ) -> Vec<(crate::types::Role, String)> {
        let skill_budget = SkillCatalogBudget::resolve(
            context_window,
            self.skill_settings.catalog_budget(),
            self.skill_settings.description_budget(),
        );
        build_initial_prompt_messages_with_mcp(
            &self.current_dir,
            self.config_dir,
            self.cli_system_prompt,
            self.settings_system_prompt,
            self.agents_files,
            self.skill_catalog,
            skill_budget,
            self.tool_context.sandbox_policy(),
            self.toolbox_tools,
            self.mcp_servers,
            self.tools_enabled,
            supports_images,
            self.include_runtime_context,
            self.include_project_prompt,
        )
    }
}

/// Load the prior session a fork starts from.
fn fork_source(
    run_mode: &RunMode,
    data_dir: &DataDir,
    current_dir: &Path,
) -> anyhow::Result<Session> {
    match run_mode {
        RunMode::ForkLatest => data_dir
            .load_latest_session(current_dir)?
            .ok_or_else(|| anyhow::anyhow!("No previous session found for this directory")),
        RunMode::Fork { session_id } => data_dir
            .load_session(*session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session {session_id} not found")),
        _ => unreachable!("fork arm only handles fork modes"),
    }
}
