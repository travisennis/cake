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
use crate::clients::mcp::{LazyMcpRuntime, McpServer};
use crate::clients::{Agent, ToolContext};
use crate::config::provider_identity::{self, SessionProviderIdentityCheck};
use crate::config::settings::{ResolvedImagePolicy, SkillSettings};
use crate::config::skills::Skill;
use crate::config::toolbox::ToolboxTool;
use crate::config::{
    AgentsFile, DataDir, ModelConfig, ModelDefinition, ResolvedModelConfig, Session, SkillCatalog,
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
    /// Build the agent/session pair for the selected run mode.
    pub(crate) fn build_client_and_session(
        &self,
        run_mode: &RunMode,
        data_dir: &DataDir,
        inputs: RunInputs<'_>,
    ) -> anyhow::Result<RunSession> {
        match run_mode {
            RunMode::Resume {
                session_id,
                read_only,
            } => self.resume_run(*session_id, *read_only, data_dir, inputs),
            RunMode::ForkLatest | RunMode::Fork { .. } => {
                self.forked_run(run_mode, data_dir, inputs)
            },
            RunMode::NewSession | RunMode::Ephemeral => self.new_run(
                inputs,
                run_mode
                    .persists_session()
                    .then_some(SessionPersistencePlan::Create {
                        seed_records: Vec::new(),
                        blob_source: None,
                    }),
            ),
        }
    }

    /// Apply model overrides and build the pre-prompt state shared by every
    /// run mode.
    fn prepare_assembly<'a>(
        &self,
        inputs: RunInputs<'a>,
        config: ModelConfig,
        model_config_name: String,
        provider_identity: ProviderIdentity,
        session_model_change_override: bool,
    ) -> anyhow::Result<SessionAssembly<'a>> {
        let resolved = ResolvedModelConfig::resolve(self.apply_cli_overrides_to_config(config))?;
        let initial_messages = inputs.initial_messages(
            resolved.model_config.supports_images,
            resolved.model_config.context_window,
        );
        Ok(SessionAssembly {
            inputs,
            resolved,
            model_config_name,
            effective_provider_identity: provider_identity,
            session_model_change_override,
            initial_messages,
        })
    }

    /// Resolve a restored session's model, verify its pinned provider
    /// identity, and rebuild the pair.
    fn restored_run(
        &self,
        restored: Session,
        inputs: RunInputs<'_>,
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
        self.prepare_assembly(
            inputs,
            config,
            model_config_name,
            provider_identity,
            session_model_change_override,
        )?
        .assemble(RunOrigin::Restored {
            restored: Box::new(restored),
            persistence,
        })
    }

    /// `--resume <UUID>`: restore the named session.
    fn resume_run(
        &self,
        session_id: uuid::Uuid,
        read_only: bool,
        data_dir: &DataDir,
        inputs: RunInputs<'_>,
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
        inputs: RunInputs<'_>,
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
        self.prepare_assembly(
            inputs,
            config,
            model_config_name,
            provider_identity,
            session_model_change_override,
        )?
        .assemble(RunOrigin::Forked {
            restored: &restored,
        })
    }

    /// New and ephemeral runs: start fresh from the CLI-selected model.
    fn new_run(
        &self,
        inputs: RunInputs<'_>,
        persistence: Option<SessionPersistencePlan>,
    ) -> anyhow::Result<RunSession> {
        let (config, model_config_name) =
            self.resolve_configured_model(inputs.models, inputs.default_model)?;
        let provider_identity = provider_identity::build(&config, &model_config_name)?;
        self.prepare_assembly(inputs, config, model_config_name, provider_identity, false)?
            .assemble(RunOrigin::New { persistence })
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
pub struct RunInputs<'a> {
    pub current_dir: PathBuf,
    pub config_dir: &'a Path,
    pub cli_system_prompt: Option<&'a Path>,
    pub settings_system_prompt: Option<&'a Path>,
    pub agents_files: &'a [AgentsFile],
    pub skill_catalog: &'a SkillCatalog,
    pub models: &'a HashMap<String, ModelDefinition>,
    pub images: &'a ResolvedImagePolicy,
    pub default_model: Option<&'a str>,
    pub skill_settings: &'a SkillSettings,
    pub tool_context: &'a Arc<ToolContext>,
    pub toolbox_tools: &'a [ToolboxTool],
    pub mcp_servers: &'a [McpServer],
    pub lazy_mcp: Option<&'a Arc<LazyMcpRuntime>>,
    pub tools_enabled: Option<&'a [String]>,
    pub include_runtime_context: bool,
    pub include_project_prompt: bool,
    pub task_id: uuid::Uuid,
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
    ) -> Vec<(Role, String)> {
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
            self.lazy_mcp.map(Arc::as_ref),
            self.tools_enabled,
            supports_images,
            self.include_runtime_context,
            self.include_project_prompt,
        )
    }
}

/// The state that distinguishes a new, restored, or forked run.
enum RunOrigin<'a> {
    New {
        persistence: Option<SessionPersistencePlan>,
    },
    Restored {
        restored: Box<Session>,
        persistence: Option<SessionPersistencePlan>,
    },
    Forked {
        restored: &'a Session,
    },
}

/// Session fields selected by the run origin before the shared assembly step.
struct SessionSeed {
    id: uuid::Uuid,
    working_dir: PathBuf,
    provider_identity: Option<ProviderIdentity>,
    context_identity: Option<ContextIdentity>,
    stored_prompt_prefix: Option<StoredPromptPrefix>,
    prompt_prefix_decision: PromptPrefixDecision,
    persistence: Option<SessionPersistencePlan>,
    session_model_change_override: bool,
}

/// One owner for the shared Agent and Session builder chain.
struct SessionAssembly<'a> {
    inputs: RunInputs<'a>,
    resolved: ResolvedModelConfig,
    model_config_name: String,
    effective_provider_identity: ProviderIdentity,
    session_model_change_override: bool,
    initial_messages: Vec<(Role, String)>,
}

impl SessionAssembly<'_> {
    fn base_agent(&self) -> Agent {
        Agent::new(self.resolved.clone(), &self.initial_messages)
            .with_task_id(self.inputs.task_id)
            .with_model_config_name(self.model_config_name.clone())
            .with_tool_context(Arc::clone(self.inputs.tool_context))
            .with_toolbox_tools(self.inputs.toolbox_tools.to_vec())
            .with_mcp_servers(self.inputs.mcp_servers.to_vec())
            .with_lazy_mcp_servers(self.inputs.lazy_mcp.cloned())
            .with_enabled_tools(self.inputs.tools_enabled)
            .with_skill_locations(skill_locations(self.inputs.skill_catalog))
            .with_image_policy(self.inputs.images)
    }

    fn assemble(self, origin: RunOrigin<'_>) -> anyhow::Result<RunSession> {
        let mut agent = self.base_agent();
        let seed = match origin {
            RunOrigin::New { persistence } => {
                let id = agent.session_id();
                info!(target: "cake", "New session: {id}");
                SessionSeed {
                    id,
                    working_dir: self.inputs.current_dir.clone(),
                    provider_identity: Some(self.effective_provider_identity.clone()),
                    context_identity: None,
                    stored_prompt_prefix: None,
                    prompt_prefix_decision: PromptPrefixDecision::Initial,
                    persistence,
                    session_model_change_override: false,
                }
            },
            RunOrigin::Restored {
                restored,
                persistence,
            } => {
                let restored = *restored;
                let stored_prompt_prefix = restored.stored_prompt_prefix();
                let id = restored.id;
                agent = agent
                    .with_session_id(id)
                    .with_active_mcp_tools(restored.activated_mcp_tools())
                    .with_context_seed(restored.context_seed())
                    .map_err(|error| anyhow::anyhow!("Cannot restore session {id}: {error:#}"))?
                    .with_last_usage(restored.last_turn_usage())
                    .with_activated_skills(restored.activated_skills());
                warn_omitted_history_images(&agent, &self.resolved, &self.model_config_name);
                SessionSeed {
                    id,
                    working_dir: restored.working_dir,
                    provider_identity: restored.provider_identity,
                    context_identity: restored.context_identity,
                    stored_prompt_prefix,
                    prompt_prefix_decision: PromptPrefixDecision::Rebuilt,
                    persistence,
                    session_model_change_override: self.session_model_change_override,
                }
            },
            RunOrigin::Forked { restored } => {
                let stored_prompt_prefix = restored.stored_prompt_prefix();
                let id = agent.session_id();
                agent = agent
                    .with_active_mcp_tools(restored.activated_mcp_tools())
                    .with_context_seed(restored.context_seed().for_session(id.to_string()))
                    .map_err(|error| {
                        anyhow::anyhow!("Cannot fork session {}: {error:#}", restored.id)
                    })?
                    .with_last_usage(restored.last_turn_usage())
                    .with_activated_skills(restored.activated_skills());
                warn_omitted_history_images(&agent, &self.resolved, &self.model_config_name);
                info!(target: "cake", "New forked session: {id}");
                SessionSeed {
                    id,
                    working_dir: self.inputs.current_dir.clone(),
                    provider_identity: restored.provider_identity.clone(),
                    context_identity: None,
                    stored_prompt_prefix,
                    prompt_prefix_decision: PromptPrefixDecision::Rebuilt,
                    persistence: Some(SessionPersistencePlan::Create {
                        seed_records: fork_seed_records(restored, id),
                        blob_source: Some(restored.id),
                    }),
                    session_model_change_override: self.session_model_change_override,
                }
            },
        };
        agent.set_prompt_prefix_runtime(self.inputs.include_runtime_context);
        let mut session = Session::new(seed.id, seed.working_dir);
        session.model = Some(self.resolved.model_config.model.clone());
        session.model_config = Some(self.model_config_name);
        session.provider_identity = seed.provider_identity;
        session.context_identity = seed.context_identity;
        session.system_prompt = self
            .initial_messages
            .first()
            .map(|(_, content)| content.clone());
        Ok(RunSession {
            agent,
            session,
            persistence: seed.persistence,
            effective_provider_identity: Some(self.effective_provider_identity),
            session_model_change_override: seed.session_model_change_override,
            stored_prompt_prefix: seed.stored_prompt_prefix,
            include_runtime_context: self.inputs.include_runtime_context,
            prompt_prefix_decision: seed.prompt_prefix_decision,
            resolved: Box::new(self.resolved),
        })
    }
}

/// Copy the prior transcript records a fork seeds into its new session.
fn fork_seed_records(restored: &Session, new_id: uuid::Uuid) -> Vec<SessionRecord> {
    let latest_checkpoint = restored.latest_checkpoint_index();
    restored
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
        .collect()
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
