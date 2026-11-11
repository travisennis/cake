//! Session construction from CLI arguments.
//!
//! Provides the free function [`skill_locations`] and
//! `impl CodingAssistant` methods that build agent/session pairs
//! for new, restored, and forked runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::info;

use crate::cli::run_mode::{RunMode, SessionPersistencePlan};
use crate::clients::mcp::McpServer;
use crate::clients::{Agent, ToolContext};
use crate::config::provider_identity::{self, SessionProviderIdentityCheck};
use crate::config::settings::SkillSettings;
use crate::config::skills::Skill;
use crate::config::toolbox::ToolboxTool;
use crate::config::{
    AgentsFile, DataDir, ModelDefinition, ResolvedModelConfig, Session, SkillCatalog,
    SkillCatalogBudget,
};
use crate::prompts::build_initial_prompt_messages_with_mcp;
use crate::types::{ProviderIdentity, SessionRecord};

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
        initial_messages: &[(crate::types::Role, String)],
        skill_locations: &HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        toolbox_tools: Vec<ToolboxTool>,
        mcp_servers: Vec<McpServer>,
        enabled_tools: Option<&[String]>,
        task_id: uuid::Uuid,
    ) -> anyhow::Result<RunSession> {
        let seed = restored.context_seed();
        // Seed once-per-session activation dedup from the persisted records so
        // a resumed run does not re-emit "first observed" for known skills.
        let activated_skills = restored.activated_skills();
        let activated_mcp_tools = restored.activated_mcp_tools();

        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_session_id(restored.id)
            .with_task_id(task_id)
            .with_model_config_name(model_config_name.clone())
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_mcp_servers(mcp_servers)
            .with_enabled_tools(enabled_tools)
            .with_active_mcp_tools(activated_mcp_tools)
            // Flattened rather than layered: the CLI prints only the outermost
            // error, and the underlying diagnostic is the useful part.
            .with_context_seed(seed)
            .map_err(|error| anyhow::anyhow!("Cannot restore session {}: {error:#}", restored.id))?
            .with_last_usage(restored.last_turn_usage())
            .with_skill_locations(skill_locations.clone())
            .with_activated_skills(activated_skills);
        warn_omitted_history_images(&agent, &resolved, &model_config_name);
        let mut session = Session::new(restored.id, restored.working_dir);
        session.model = Some(resolved.model_config.model);
        session.model_config = Some(model_config_name);
        session.provider_identity = restored.provider_identity;
        Ok(RunSession {
            agent,
            session,
            persistence: Some(SessionPersistencePlan::Append),
            effective_provider_identity: Some(effective_provider_identity),
            session_model_change_override,
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
        initial_messages: &[(crate::types::Role, String)],
        skill_locations: HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        toolbox_tools: Vec<ToolboxTool>,
        mcp_servers: Vec<McpServer>,
        enabled_tools: Option<&[String]>,
        task_id: uuid::Uuid,
        persistence: Option<SessionPersistencePlan>,
    ) -> RunSession {
        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_task_id(task_id)
            .with_model_config_name(model_config_name.clone())
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_mcp_servers(mcp_servers)
            .with_enabled_tools(enabled_tools)
            .with_skill_locations(skill_locations);
        let new_id = agent.session_id();
        info!(target: "cake", "New session: {new_id}");
        let mut session = Session::new(new_id, current_dir);
        session.model = Some(resolved.model_config.model);
        session.model_config = Some(model_config_name);
        session.provider_identity = Some(provider_identity.clone());
        session.system_prompt = initial_messages.first().map(|(_, content)| content.clone());
        RunSession {
            agent,
            session,
            persistence,
            effective_provider_identity: Some(provider_identity),
            session_model_change_override: false,
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
        initial_messages: &[(crate::types::Role, String)],
        skill_locations: HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        toolbox_tools: Vec<ToolboxTool>,
        mcp_servers: Vec<McpServer>,
        enabled_tools: Option<&[String]>,
        task_id: uuid::Uuid,
    ) -> anyhow::Result<RunSession> {
        let activated_mcp_tools = restored.activated_mcp_tools();
        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_task_id(task_id)
            .with_model_config_name(model_config_name.clone())
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_mcp_servers(mcp_servers)
            .with_enabled_tools(enabled_tools)
            .with_active_mcp_tools(activated_mcp_tools);
        let new_id = agent.session_id();
        // Re-envelope the seed's checkpoint for this session before restoring
        // it, so the in-memory checkpoint and the copied record below agree.
        let agent = agent
            .with_context_seed(restored.context_seed().for_session(new_id.to_string()))
            .map_err(|error| anyhow::anyhow!("Cannot fork session {}: {error:#}", restored.id))?
            .with_last_usage(restored.last_turn_usage())
            .with_skill_locations(skill_locations)
            // The fork's session file starts with copies of the source
            // session's SkillActivated records; seed the same names so re-reads
            // do not emit duplicates.
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
                // Copy only the latest checkpoint, re-enveloped for this
                // session at its original position among the copied records,
                // so the positional boundary is reconstructed. Checkpoint-free
                // format-4 forks copy exactly what they copied before.
                SessionRecord::Compaction(checkpoint) if latest_checkpoint == Some(index) => Some(
                    SessionRecord::Compaction(checkpoint.for_session(new_id.to_string())),
                ),
                _ => None,
            })
            .collect();
        info!(target: "cake", "New forked session: {new_id}");
        let mut session = Session::new(new_id, current_dir);
        session.model = Some(resolved.model_config.model);
        session.model_config = Some(model_config_name);
        // A fork copies the parent snapshot. `--allow-session-model-change`
        // accepts the current resolution for this invocation without
        // rebinding the fork to a new destination.
        session
            .provider_identity
            .clone_from(&restored.provider_identity);
        session.system_prompt = initial_messages.first().map(|(_, content)| content.clone());
        Ok(RunSession {
            agent,
            session,
            persistence: Some(SessionPersistencePlan::Create { seed_records }),
            effective_provider_identity: Some(effective_provider_identity),
            session_model_change_override,
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
        default_model: Option<&str>,
        skill_catalog: &SkillCatalog,
        tool_context: &Arc<ToolContext>,
        toolbox_tools: &[ToolboxTool],
        mcp_servers: &[McpServer],
        enabled_tools: Option<&[String]>,
        task_id: uuid::Uuid,
        loaded_system_prompt: Option<&str>,
        skill_settings: &SkillSettings,
        include_project_prompt: bool,
    ) -> anyhow::Result<RunSession> {
        let inputs = RunInputs {
            current_dir,
            config_dir,
            cli_system_prompt: self.system_prompt.as_deref().map(std::path::Path::new),
            settings_system_prompt: loaded_system_prompt.map(std::path::Path::new),
            agents_files,
            skill_catalog,
            skill_locations: skill_locations(skill_catalog),
            models,
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
            RunMode::Resume { session_id } => self.resume_run(*session_id, data_dir, &inputs),
            RunMode::ForkLatest | RunMode::Fork { .. } => {
                self.forked_run(run_mode, data_dir, &inputs)
            },
            RunMode::NewSession | RunMode::Ephemeral => self.new_run(
                &inputs,
                run_mode
                    .persists_session()
                    .then_some(SessionPersistencePlan::Create {
                        seed_records: Vec::new(),
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
        Self::restored_client_and_session(
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
            inputs.task_id,
        )
    }

    /// `--resume <UUID>`: restore the named session.
    fn resume_run(
        &self,
        session_id: uuid::Uuid,
        data_dir: &DataDir,
        inputs: &RunInputs<'_>,
    ) -> anyhow::Result<RunSession> {
        let restored = data_dir
            .load_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session {session_id} not found"))?;
        info!(target: "cake", "Resumed session: {}", restored.id);
        self.restored_run(restored, inputs)
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
        Self::forked_client_and_session(
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
            inputs.task_id,
        )
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
        Ok(Self::new_client_and_session(
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
            inputs.task_id,
            persistence,
        ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SandboxPolicy;
    use crate::config::settings::ModelDefinition;
    use crate::config::skills::SkillCatalog;
    use crate::types::ImagePart;
    use clap::Parser;
    use std::collections::HashSet;

    fn test_resolved_model_config() -> ResolvedModelConfig {
        ResolvedModelConfig {
            model_config: crate::config::model::ModelConfig {
                supports_images: false,
                model: "test".to_string(),
                api_type: crate::config::model::ApiType::ChatCompletions,
                base_url: "https://example.invalid/v1".to_string(),
                api_key_env: "SESSION_FACTORY_TEST_KEY".to_string(),
                provider: None,
                provider_headers: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                context_window: None,
                reasoning_effort: None,
                reasoning_summary: None,
                reasoning_max_tokens: None,
                providers: vec![],
            },
            api_key: "test-key".to_string(),
        }
    }

    fn test_provider_identity() -> ProviderIdentity {
        provider_identity::build(&test_models()["test"].to_model_config(), "test")
            .expect("test model should have a provider identity")
    }

    fn test_tool_context() -> Arc<ToolContext> {
        Arc::new(ToolContext::new(
            PathBuf::from("/work"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            SandboxPolicy::WorkspaceWrite,
        ))
    }

    fn session_with_activated_skill(id: uuid::Uuid) -> Session {
        let mut restored = Session::new(id, PathBuf::from("/work"));
        restored.records.push(SessionRecord::SkillActivated {
            session_id: id.to_string(),
            task_id: "task-1".to_string(),
            timestamp: chrono::Utc::now(),
            name: "debugging-cake".to_string(),
            path: PathBuf::from("/work/.agents/skills/debugging-cake/SKILL.md"),
        });
        restored
    }

    fn session_with_activated_mcp_tool(id: uuid::Uuid) -> Session {
        let mut restored = Session::new(id, PathBuf::from("/work"));
        restored.records.push(SessionRecord::McpToolsActivated {
            session_id: id.to_string(),
            task_id: "task-1".to_string(),
            timestamp: chrono::Utc::now(),
            tools: vec!["mcp__fixture__echo".to_string()],
        });
        restored
    }

    #[test]
    fn omitted_image_warning_names_the_count_and_model_identity() {
        let warning = super::omitted_history_images_warning(false, 2, "glm-5.1", "fast")
            .expect("a text-only model with images should warn");
        assert!(warning.contains("'glm-5.1'"));
        assert!(warning.contains("'fast'"));
        assert!(warning.contains("omitting 2 images"));

        let singular = super::omitted_history_images_warning(false, 1, "glm-5.1", "fast")
            .expect("one image should still warn");
        assert!(singular.contains("omitting 1 image "));
    }

    #[test]
    fn omitted_image_warning_is_silent_without_images_or_with_image_support() {
        assert!(super::omitted_history_images_warning(false, 0, "glm-5.1", "fast").is_none());
        assert!(super::omitted_history_images_warning(true, 2, "gpt-5", "vision").is_none());
    }

    #[test]
    fn restored_session_keeps_images_when_the_model_cannot_accept_them() {
        let id = uuid::Uuid::new_v4();
        let mut restored = Session::new(id, PathBuf::from("/work"));
        restored.records.push(SessionRecord::Message(MessageData {
            role: crate::types::Role::User,
            content: "What is in this picture?".to_string(),
            id: None,
            status: None,
            timestamp: None,
            images: vec![ImagePart {
                media_type: "image/png".to_string(),
                data_base64: "QUJD".to_string(),
            }],
        }));

        let run = crate::CodingAssistant::restored_client_and_session(
            restored,
            test_resolved_model_config(),
            "test".to_string(),
            test_provider_identity(),
            false,
            &[(crate::types::Role::System, "test".to_string())],
            &HashMap::new(),
            test_tool_context(),
            Vec::new(),
            Vec::new(),
            None,
            uuid::Uuid::new_v4(),
        )
        .expect("restore should succeed");

        assert_eq!(
            run.agent.history_image_count(),
            1,
            "omission happens at request build; the restored session keeps its images"
        );
    }

    /// A session is built before the asynchronous policy-gate startup, so its
    /// initial tool context must not claim a gate that has not started yet.
    #[test]
    fn new_session_defers_gate_startup() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let cli = crate::CodingAssistant::parse_from(["cake"]);
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");
            let config_dir = tempfile::tempdir().expect("temp config dir");
            let models = HashMap::from([(
                "test".to_string(),
                ModelDefinition {
                    supports_images: false,
                    name: "test".to_string(),
                    model: "glm-5.1".to_string(),
                    base_url: "https://example.invalid/v1".to_string(),
                    api_key_env: "SESSION_FACTORY_TEST_KEY".to_string(),
                    provider: None,
                    provider_headers: None,
                    api_type: crate::config::model::ApiType::Responses,
                    temperature: None,
                    top_p: None,
                    top_k: None,
                    max_output_tokens: None,
                    context_window: None,
                    reasoning_effort: None,
                    reasoning_summary: None,
                    reasoning_max_tokens: None,
                    providers: vec![],
                },
            )]);
            let tool_context = Arc::new(ToolContext::new(
                working_dir.path().to_path_buf(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                SandboxPolicy::WorkspaceWrite,
            ));

            let run_session = cli
                .build_client_and_session(
                    &RunMode::NewSession,
                    &data_dir,
                    working_dir.path().to_path_buf(),
                    config_dir.path(),
                    &[],
                    &models,
                    Some("test"),
                    &SkillCatalog {
                        skills: Vec::new(),
                        diagnostics: Vec::new(),
                    },
                    &tool_context,
                    &[],
                    &[],
                    None,
                    uuid::Uuid::new_v4(),
                    None,
                    &SkillSettings::default(),
                    true,
                )
                .expect("session construction should succeed");

            assert!(
                run_session.agent.tool_context().gates.is_none(),
                "external gates initialize asynchronously after session construction"
            );
        });
    }

    #[test]
    fn restored_session_seeds_agent_last_usage() {
        let mut restored = Session::new(uuid::Uuid::new_v4(), PathBuf::from("/work"));
        restored.model = Some("test".to_string());
        let last_usage = crate::types::Usage {
            input_tokens: 1234,
            output_tokens: 100,
            total_tokens: 1334,
            ..crate::types::Usage::default()
        };
        restored
            .records
            .push(crate::types::SessionRecord::TurnUsage(
                crate::types::TurnUsageData {
                    session_id: restored.id.to_string(),
                    task_id: "task-1".to_string(),
                    turn: 3,
                    usage: last_usage,
                    timestamp: chrono::Utc::now(),
                    attempt: None,
                    terminal_class: None,
                    usage_presence: None,
                    model: None,
                    response_model: None,
                    provider_request_id: None,
                    context_window: None,
                    remaining_context_tokens: None,
                },
            ));

        let resolved = ResolvedModelConfig {
            model_config: crate::config::model::ModelConfig {
                supports_images: false,
                model: "test".to_string(),
                api_type: crate::config::model::ApiType::ChatCompletions,
                base_url: "https://example.invalid/v1".to_string(),
                api_key_env: "SESSION_FACTORY_TEST_KEY".to_string(),
                provider: None,
                provider_headers: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                context_window: Some(200_000),
                reasoning_effort: None,
                reasoning_summary: None,
                reasoning_max_tokens: None,
                providers: vec![],
            },
            api_key: "test-key".to_string(),
        };
        let tool_context = Arc::new(ToolContext::new(
            PathBuf::from("/work"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            crate::SandboxPolicy::WorkspaceWrite,
        ));

        let run = crate::CodingAssistant::restored_client_and_session(
            restored,
            resolved,
            "test".to_string(),
            test_provider_identity(),
            false,
            &[(crate::types::Role::System, "test".to_string())],
            &HashMap::new(),
            tool_context,
            Vec::new(),
            Vec::new(),
            None,
            uuid::Uuid::new_v4(),
        )
        .expect("restore should succeed");

        // The restored agent knows its current context size before the first
        // new provider call, seeded from the session's last per-turn usage.
        let restored_last = run.agent.last_usage().unwrap();
        assert_eq!(restored_last.input_tokens, 1234);
        assert_eq!(restored_last.output_tokens, 100);
        assert_eq!(restored_last.total_tokens, 1334);
        assert_eq!(run.agent.context_remaining_tokens(), Some(200_000 - 1234));
    }

    #[test]
    fn restored_session_seeds_agent_activated_skills() {
        let restored = session_with_activated_skill(uuid::Uuid::new_v4());

        let run = crate::CodingAssistant::restored_client_and_session(
            restored,
            test_resolved_model_config(),
            "test".to_string(),
            test_provider_identity(),
            false,
            &[(crate::types::Role::System, "test".to_string())],
            &HashMap::new(),
            test_tool_context(),
            Vec::new(),
            Vec::new(),
            None,
            uuid::Uuid::new_v4(),
        )
        .expect("restore should succeed");

        // The resumed agent knows which skills were already activated, so a
        // re-read cannot emit a duplicate "first observed" record.
        assert_eq!(
            run.agent.activated_skill_names(),
            HashSet::from(["debugging-cake".to_string()])
        );
    }

    #[test]
    fn forked_session_seeds_agent_activated_skills() {
        let restored = session_with_activated_skill(uuid::Uuid::new_v4());

        let run = crate::CodingAssistant::forked_client_and_session(
            &restored,
            test_resolved_model_config(),
            "test".to_string(),
            test_provider_identity(),
            false,
            PathBuf::from("/work"),
            &[(crate::types::Role::System, "test".to_string())],
            HashMap::new(),
            test_tool_context(),
            Vec::new(),
            Vec::new(),
            None,
            uuid::Uuid::new_v4(),
        )
        .expect("fork should succeed");

        // The fork inherits the activated set and carries the historical
        // activation records into its own session file.
        assert_eq!(
            run.agent.activated_skill_names(),
            HashSet::from(["debugging-cake".to_string()])
        );
        assert!(matches!(
            run.persistence.as_ref(),
            Some(SessionPersistencePlan::Create { seed_records })
                if matches!(
                    seed_records.first(),
                    Some(SessionRecord::SkillActivated { name, .. }) if name == "debugging-cake"
                )
        ));
    }

    #[test]
    fn forked_session_copies_mcp_activation_records() {
        let restored = session_with_activated_mcp_tool(uuid::Uuid::new_v4());

        let run = crate::CodingAssistant::forked_client_and_session(
            &restored,
            test_resolved_model_config(),
            "test".to_string(),
            test_provider_identity(),
            false,
            PathBuf::from("/work"),
            &[(crate::types::Role::System, "test".to_string())],
            HashMap::new(),
            test_tool_context(),
            Vec::new(),
            Vec::new(),
            None,
            uuid::Uuid::new_v4(),
        )
        .expect("fork should succeed");

        let new_session_id = run.agent.session_id().to_string();
        let Some(SessionPersistencePlan::Create { seed_records }) = run.persistence.as_ref() else {
            panic!("a fork persists a new session");
        };
        let activation = seed_records
            .iter()
            .find_map(|record| match record {
                SessionRecord::McpToolsActivated {
                    session_id, tools, ..
                } => Some((session_id, tools)),
                _ => None,
            })
            .expect("the fork should seed the MCP activation record");
        assert_eq!(activation.0, &new_session_id);
        assert_eq!(activation.1.clone(), vec!["mcp__fixture__echo".to_string()]);
    }

    // ---- build_client_and_session run-mode coverage ----

    fn test_models() -> HashMap<String, ModelDefinition> {
        HashMap::from([(
            "test".to_string(),
            ModelDefinition {
                supports_images: false,
                name: "test".to_string(),
                model: "glm-5.1".to_string(),
                base_url: "https://example.invalid/v1".to_string(),
                api_key_env: "SESSION_FACTORY_TEST_KEY".to_string(),
                provider: None,
                provider_headers: None,
                api_type: crate::config::model::ApiType::ChatCompletions,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                context_window: None,
                reasoning_effort: None,
                reasoning_summary: None,
                reasoning_max_tokens: None,
                providers: vec![],
            },
        )])
    }

    /// Persist a minimal restorable session for `working_dir`.
    fn saved_session(data_dir: &DataDir, working_dir: &Path) -> Session {
        let mut session = Session::new(uuid::Uuid::new_v4(), working_dir.to_path_buf());
        session.model = Some("glm-5.1".to_string());
        data_dir
            .save_session(&session)
            .expect("session fixture should save");
        session
    }

    /// Persist a session pinned to the test model's current identity.
    fn saved_session_with_identity(data_dir: &DataDir, working_dir: &Path) -> Session {
        let mut session = Session::new(uuid::Uuid::new_v4(), working_dir.to_path_buf());
        session.model = Some("glm-5.1".to_string());
        session.model_config = Some("test".to_string());
        session.provider_identity = Some(test_provider_identity());
        data_dir
            .save_session(&session)
            .expect("session fixture should save");
        session
    }

    /// Call `build_client_and_session` with the shared test fixtures.
    fn build_for_mode(
        cli: &crate::CodingAssistant,
        mode: &RunMode,
        data_dir: &DataDir,
        working_dir: &Path,
    ) -> anyhow::Result<RunSession> {
        let tool_context = Arc::new(ToolContext::new(
            working_dir.to_path_buf(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            SandboxPolicy::WorkspaceWrite,
        ));
        cli.build_client_and_session(
            mode,
            data_dir,
            working_dir.to_path_buf(),
            working_dir,
            &[],
            &test_models(),
            Some("test"),
            &SkillCatalog {
                skills: Vec::new(),
                diagnostics: Vec::new(),
            },
            &tool_context,
            &[],
            &[],
            None,
            uuid::Uuid::new_v4(),
            None,
            &SkillSettings::default(),
            true,
        )
    }

    /// Flatten the provider request context into text for prompt assertions.
    fn request_text(run: &RunSession) -> String {
        run.agent
            .request_context()
            .into_iter()
            .filter_map(|item| match item {
                crate::types::ConversationItem::Message { content, .. } => Some(content),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn no_runtime_context_flag_omits_environment_message_from_request() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");

            let default_run = build_for_mode(
                &crate::CodingAssistant::parse_from(["cake"]),
                &RunMode::NewSession,
                &data_dir,
                working_dir.path(),
            )
            .expect("default session construction should succeed");
            let no_context_run = build_for_mode(
                &crate::CodingAssistant::parse_from(["cake", "--no-runtime-context"]),
                &RunMode::NewSession,
                &data_dir,
                working_dir.path(),
            )
            .expect("no-runtime-context session construction should succeed");

            let default_text = request_text(&default_run);
            assert!(default_text.contains("Current working directory:"));
            assert!(default_text.contains("Today's date:"));

            let no_context_text = request_text(&no_context_run);
            for label in [
                "Current working directory",
                "Today's date",
                "Platform:",
                "Architecture:",
                "Shell:",
                "Terminal:",
            ] {
                assert!(
                    !no_context_text.contains(label),
                    "runtime label {label:?} reached the request"
                );
            }
            assert!(no_context_text.contains("You are cake."));
            assert!(no_context_text.contains("## Available tools"));
        });
    }

    /// A resumed run rebuilds the leading prompt context instead of replaying
    /// the Developer message an earlier invocation persisted.
    #[test]
    fn resumed_run_rebuilds_prompt_context_without_runtime_message() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");

            let mut saved = Session::new(uuid::Uuid::new_v4(), working_dir.path().to_path_buf());
            saved.model = Some("glm-5.1".to_string());
            saved
                .records
                .push(SessionRecord::Message(crate::types::session::MessageData {
                    role: crate::types::Role::Developer,
                    content: "Current working directory: /stale\nToday's date: yesterday"
                        .to_string(),
                    id: None,
                    status: None,
                    timestamp: None,
                    images: Vec::new(),
                }));
            data_dir
                .save_session(&saved)
                .expect("session fixture should save");

            let run = build_for_mode(
                &crate::CodingAssistant::parse_from(["cake", "--no-runtime-context"]),
                &RunMode::Resume {
                    session_id: saved.id,
                },
                &data_dir,
                working_dir.path(),
            )
            .expect("resume should succeed");

            let text = request_text(&run);
            assert!(
                !text.contains("Current working directory"),
                "persisted runtime context was replayed"
            );
            assert!(
                !text.contains("Today's date"),
                "persisted runtime context was replayed"
            );
            assert!(text.contains("You are cake."));
        });
    }

    #[test]
    fn resume_restores_the_named_session() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let cli = crate::CodingAssistant::parse_from(["cake"]);
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");
            let saved = saved_session(&data_dir, working_dir.path());

            let run = build_for_mode(
                &cli,
                &RunMode::Resume {
                    session_id: saved.id,
                },
                &data_dir,
                working_dir.path(),
            )
            .expect("resume should succeed");

            assert_eq!(run.agent.session_id(), saved.id);
            assert!(matches!(
                run.persistence,
                Some(SessionPersistencePlan::Append)
            ));
        });
    }

    #[test]
    fn new_session_persists_the_current_provider_identity() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");

            let run = build_for_mode(
                &crate::CodingAssistant::parse_from(["cake"]),
                &RunMode::NewSession,
                &data_dir,
                working_dir.path(),
            )
            .expect("new session construction should succeed");

            let stored = run
                .session
                .provider_identity
                .clone()
                .expect("a new session pins provider identity");
            assert_eq!(stored.model_config, "test");
            assert_eq!(stored.base_url, "https://example.invalid/v1");
            assert_eq!(
                stored.credential,
                crate::types::provider_identity::ProviderCredentialSource::Env {
                    name: "SESSION_FACTORY_TEST_KEY".to_string()
                }
            );
            assert_eq!(run.effective_provider_identity.as_ref(), Some(&stored));
            assert!(!run.session_model_change_override);
        });
    }

    #[test]
    fn resume_with_matching_identity_proceeds_without_an_override() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");
            let saved = saved_session_with_identity(&data_dir, working_dir.path());

            let run = build_for_mode(
                &crate::CodingAssistant::parse_from(["cake"]),
                &RunMode::Resume {
                    session_id: saved.id,
                },
                &data_dir,
                working_dir.path(),
            )
            .expect("matching identity should resume");

            assert!(!run.session_model_change_override);
            assert_eq!(run.session.provider_identity, saved.provider_identity);
        });
    }

    #[test]
    fn resume_with_changed_endpoint_fails_before_credential_resolution() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");
            let mut saved = Session::new(uuid::Uuid::new_v4(), working_dir.path().to_path_buf());
            saved.model = Some("glm-5.1".to_string());
            saved.model_config = Some("test".to_string());
            saved.provider_identity = Some(crate::types::ProviderIdentity {
                base_url: "https://old.example.invalid/v1".to_string(),
                ..test_provider_identity()
            });
            data_dir
                .save_session(&saved)
                .expect("session fixture should save");

            let error = build_for_mode(
                &crate::CodingAssistant::parse_from(["cake"]),
                &RunMode::Resume {
                    session_id: saved.id,
                },
                &data_dir,
                working_dir.path(),
            )
            .err()
            .expect("changed endpoint should fail");
            let message = error.to_string();
            assert!(message.contains("Session model mismatch"), "{message}");
            assert!(message.contains("base_url:"), "{message}");
            assert!(
                message.contains("https://old.example.invalid/v1"),
                "{message}"
            );
            assert!(message.contains("https://example.invalid/v1"), "{message}");
            assert!(!message.contains("test-key"), "{message}");
        });
    }

    #[test]
    fn allow_session_model_change_accepts_without_rewriting_the_snapshot() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");
            let old_identity = crate::types::ProviderIdentity {
                base_url: "https://old.example.invalid/v1".to_string(),
                ..test_provider_identity()
            };
            let mut saved = Session::new(uuid::Uuid::new_v4(), working_dir.path().to_path_buf());
            saved.model = Some("glm-5.1".to_string());
            saved.model_config = Some("test".to_string());
            saved.provider_identity = Some(old_identity.clone());
            data_dir
                .save_session(&saved)
                .expect("session fixture should save");

            let run = build_for_mode(
                &crate::CodingAssistant::parse_from(["cake", "--allow-session-model-change"]),
                &RunMode::Resume {
                    session_id: saved.id,
                },
                &data_dir,
                working_dir.path(),
            )
            .expect("the override should accept the changed identity");

            assert!(run.session_model_change_override);
            assert_eq!(
                run.session.provider_identity, saved.provider_identity,
                "the stored snapshot is not rewritten"
            );
            assert_eq!(
                run.effective_provider_identity
                    .as_ref()
                    .map(|identity| identity.base_url.as_str()),
                Some("https://example.invalid/v1")
            );
            assert_ne!(
                Some(old_identity),
                run.effective_provider_identity,
                "the invocation runs against the current identity"
            );
        });
    }

    #[test]
    fn legacy_session_without_identity_resolves_with_an_unknown_baseline() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");
            let saved = saved_session(&data_dir, working_dir.path());

            let run = build_for_mode(
                &crate::CodingAssistant::parse_from(["cake"]),
                &RunMode::Resume {
                    session_id: saved.id,
                },
                &data_dir,
                working_dir.path(),
            )
            .expect("a legacy session still resumes");

            assert!(run.session.provider_identity.is_none());
            assert!(run.effective_provider_identity.is_some());
            assert!(!run.session_model_change_override);
        });
    }

    #[test]
    fn fork_copies_the_parent_identity_and_applies_the_check() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");
            let saved = saved_session_with_identity(&data_dir, working_dir.path());

            let run = build_for_mode(
                &crate::CodingAssistant::parse_from(["cake"]),
                &RunMode::Fork {
                    session_id: saved.id,
                },
                &data_dir,
                working_dir.path(),
            )
            .expect("matching parent identity should fork");

            assert_eq!(run.session.provider_identity, saved.provider_identity);
            assert!(!run.session_model_change_override);
        });
    }

    #[test]
    fn resume_unknown_session_reports_not_found() {
        let cli = crate::CodingAssistant::parse_from(["cake"]);
        let data_dir_dir = tempfile::tempdir().expect("temp data dir");
        let data_dir = DataDir::new_in_dir(data_dir_dir.path());
        let working_dir = tempfile::tempdir().expect("temp working dir");

        let error = build_for_mode(
            &cli,
            &RunMode::Resume {
                session_id: uuid::Uuid::new_v4(),
            },
            &data_dir,
            working_dir.path(),
        )
        .err()
        .expect("resume of unknown session should fail");

        assert!(error.to_string().contains("not found"));
    }

    #[test]
    fn fork_latest_starts_a_new_seeded_session() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let cli = crate::CodingAssistant::parse_from(["cake"]);
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");
            let mut saved = session_with_activated_skill(uuid::Uuid::new_v4());
            saved.working_dir = working_dir.path().to_path_buf();
            saved.model = Some("glm-5.1".to_string());
            data_dir
                .save_session(&saved)
                .expect("session fixture should save");

            let run = build_for_mode(&cli, &RunMode::ForkLatest, &data_dir, working_dir.path())
                .expect("fork should succeed");

            assert_ne!(run.agent.session_id(), saved.id);
            assert!(
                matches!(
                    run.persistence.as_ref(),
                    Some(SessionPersistencePlan::Create { seed_records })
                        if matches!(
                            seed_records.first(),
                            Some(SessionRecord::SkillActivated { name, .. }) if name == "debugging-cake"
                        )
                ),
                "fork should seed activation records"
            );
        });
    }

    #[test]
    fn fork_unknown_session_reports_not_found() {
        let cli = crate::CodingAssistant::parse_from(["cake"]);
        let data_dir_dir = tempfile::tempdir().expect("temp data dir");
        let data_dir = DataDir::new_in_dir(data_dir_dir.path());
        let working_dir = tempfile::tempdir().expect("temp working dir");

        let error = build_for_mode(
            &cli,
            &RunMode::Fork {
                session_id: uuid::Uuid::new_v4(),
            },
            &data_dir,
            working_dir.path(),
        )
        .err()
        .expect("fork of unknown session should fail");

        assert!(error.to_string().contains("not found"));
    }

    #[test]
    fn fork_latest_without_sessions_reports_missing_session() {
        let cli = crate::CodingAssistant::parse_from(["cake"]);
        let data_dir_dir = tempfile::tempdir().expect("temp data dir");
        let data_dir = DataDir::new_in_dir(data_dir_dir.path());
        let working_dir = tempfile::tempdir().expect("temp working dir");

        let error = build_for_mode(&cli, &RunMode::ForkLatest, &data_dir, working_dir.path())
            .err()
            .expect("fork without sessions should fail");

        assert!(error.to_string().contains("No previous session found"));
    }

    #[test]
    fn new_runs_start_with_an_empty_create_plan() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let cli = crate::CodingAssistant::parse_from(["cake"]);
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");

            let run = build_for_mode(&cli, &RunMode::NewSession, &data_dir, working_dir.path())
                .expect("new run should assemble");

            assert!(matches!(
                run.persistence,
                Some(SessionPersistencePlan::Create { seed_records }) if seed_records.is_empty()
            ));
        });
    }

    #[test]
    fn ephemeral_runs_start_without_a_persistence_plan() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let cli = crate::CodingAssistant::parse_from(["cake"]);
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");

            let run = build_for_mode(&cli, &RunMode::Ephemeral, &data_dir, working_dir.path())
                .expect("ephemeral run should assemble");

            assert!(run.persistence.is_none());
        });
    }

    /// Emit the live `task_start` record of a run's agent and return it.
    fn emitted_task_start(agent: Agent) -> serde_json::Value {
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let written = Arc::clone(&captured);
        let mut agent = agent.with_streaming_json(move |json| {
            *written.lock().expect("capture lock") = json.to_string();
        });
        agent
            .emit_task_start_record()
            .expect("task_start should emit");
        serde_json::from_str(&captured.lock().expect("capture lock"))
            .expect("task_start should be valid JSON")
    }

    #[test]
    fn new_session_task_start_reports_the_resolved_model_identity() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let cli = crate::CodingAssistant::parse_from(["cake"]);
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");

            let run = build_for_mode(&cli, &RunMode::NewSession, &data_dir, working_dir.path())
                .expect("new run should assemble");

            let task_start = emitted_task_start(run.agent);
            assert_eq!(task_start["type"], "task_start");
            assert_eq!(task_start["model"], "glm-5.1");
            assert_eq!(task_start["model_config"], "test");
        });
    }

    #[test]
    fn resumed_session_task_start_reports_the_resolved_model_identity() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let cli = crate::CodingAssistant::parse_from(["cake"]);
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");
            let saved = saved_session(&data_dir, working_dir.path());

            let run = build_for_mode(
                &cli,
                &RunMode::Resume {
                    session_id: saved.id,
                },
                &data_dir,
                working_dir.path(),
            )
            .expect("resume should succeed");

            // The fixture predates `model_config`, so the entry name is
            // resolved from the stored provider model ID. The record reports
            // the identity this run actually uses, as a new session does.
            let task_start = emitted_task_start(run.agent);
            assert_eq!(task_start["type"], "task_start");
            assert_eq!(task_start["model"], "glm-5.1");
            assert_eq!(task_start["model_config"], "test");
        });
    }

    // ---- context checkpoints ----

    use crate::types::compaction::test_support;
    use crate::types::session::{FunctionCallData, FunctionCallOutputData, MessageData};

    fn message_record(role: crate::types::Role, content: &str) -> SessionRecord {
        SessionRecord::Message(MessageData {
            role,
            content: content.to_string(),
            id: None,
            status: None,
            timestamp: None,
            images: Vec::new(),
        })
    }

    fn projected_contents(items: &[crate::types::ConversationItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                crate::types::ConversationItem::Message { content, .. } => content.clone(),
                crate::types::ConversationItem::Reasoning { .. } => "<reasoning>".to_string(),
                crate::types::ConversationItem::FunctionCall { call_id, .. } => {
                    format!("call:{call_id}")
                },
                crate::types::ConversationItem::FunctionCallOutput { call_id, .. } => {
                    format!("output:{call_id}")
                },
            })
            .collect()
    }

    fn source_with_two_checkpoints() -> Session {
        let mut source = Session::new(uuid::Uuid::new_v4(), PathBuf::from("/work"));
        let session_id = source.id.to_string();
        let records = vec![
            message_record(crate::types::Role::User, "retired request"),
            checkpoint_record(&session_id, "checkpoint-old", "older summary"),
            message_record(crate::types::Role::Assistant, "retired answer"),
            checkpoint_record(&session_id, "checkpoint-new", "newer summary"),
            message_record(crate::types::Role::User, "new request"),
            // A completed exchange after the checkpoint keeps its
            // execution-time replay declaration through the fork.
            SessionRecord::FunctionCall(FunctionCallData {
                id: "fc-later".to_string(),
                call_id: "call-later".to_string(),
                name: "Read".to_string(),
                arguments: "{}".to_string(),
                arguments_parse_error: None,
                replay: Some(crate::types::ReplaySafety::Safe),
                timestamp: None,
            }),
            SessionRecord::FunctionCallOutput(FunctionCallOutputData {
                call_id: "call-later".to_string(),
                output: "later result".to_string(),
                replay: Some(crate::types::ReplaySafety::Safe),
                timestamp: None,
                images: Vec::new(),
            }),
            // Billing, task, and activation audit records must not be copied.
            SessionRecord::TurnUsage(crate::types::TurnUsageData {
                session_id: session_id.clone(),
                task_id: uuid::Uuid::new_v4().to_string(),
                turn: 1,
                usage: crate::types::Usage::default(),
                timestamp: chrono::Utc::now(),
                attempt: None,
                terminal_class: None,
                usage_presence: None,
                model: None,
                response_model: None,
                provider_request_id: None,
                context_window: None,
                remaining_context_tokens: None,
            }),
            SessionRecord::SkillActivated {
                session_id,
                task_id: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                name: "debugging-cake".to_string(),
                path: PathBuf::from("/work/.agents/skills/debugging-cake/SKILL.md"),
            },
        ];
        source.records = records;
        source
    }

    fn checkpoint_record(session_id: &str, id: &str, summary: &str) -> SessionRecord {
        let mut data =
            test_support::checkpoint(id, summary, test_support::exchange("call-1", "out"));
        data.session_id = session_id.to_string();
        SessionRecord::Compaction(data)
    }

    /// Assert the fork seed carries only the latest checkpoint, at its
    /// original position, without duplicating audit or replay records.
    fn assert_fork_seed(seed_records: &[SessionRecord], new_session_id: &str) {
        let checkpoints: Vec<_> = seed_records
            .iter()
            .filter_map(|record| match record {
                SessionRecord::Compaction(data) => Some(data),
                _ => None,
            })
            .collect();
        assert_eq!(checkpoints.len(), 1, "only the latest checkpoint is copied");
        assert_eq!(checkpoints[0].checkpoint_id, "checkpoint-new");
        assert_eq!(
            checkpoints[0].session_id, new_session_id,
            "the copied checkpoint names the new session"
        );
        let conversation_before = seed_records
            .iter()
            .take_while(|record| !matches!(record, SessionRecord::Compaction(_)))
            .filter(|record| record.to_conversation_item().is_some())
            .count();
        assert_eq!(
            conversation_before, 2,
            "the checkpoint keeps its original position among copied records"
        );
        assert!(
            !seed_records.iter().any(|record| matches!(
                record,
                SessionRecord::TurnUsage(_) | SessionRecord::TaskComplete(_)
            )),
            "a fork must not copy billing or task audit records"
        );
        assert_eq!(
            seed_records
                .iter()
                .filter(|record| matches!(record, SessionRecord::SkillActivated { .. }))
                .count(),
            1,
            "activation records are copied once"
        );
        let copied_io: Vec<_> = seed_records
            .iter()
            .filter(|record| {
                matches!(
                    record,
                    SessionRecord::FunctionCall(data) if data.call_id == "call-later"
                ) || matches!(
                    record,
                    SessionRecord::FunctionCallOutput(data) if data.call_id == "call-later"
                )
            })
            .collect();
        assert_eq!(
            copied_io.len(),
            2,
            "a copied exchange appears once, not again through the retained tail"
        );
        assert!(
            copied_io.iter().all(|record| matches!(
                record,
                SessionRecord::FunctionCall(FunctionCallData {
                    replay: Some(_),
                    ..
                }) | SessionRecord::FunctionCallOutput(FunctionCallOutputData {
                    replay: Some(_),
                    ..
                })
            )),
            "execution-time replay declarations survive the fork"
        );
    }

    #[test]
    fn forked_session_copies_only_the_latest_checkpoint_at_its_position() {
        let source = source_with_two_checkpoints();

        let run = crate::CodingAssistant::forked_client_and_session(
            &source,
            test_resolved_model_config(),
            "test".to_string(),
            test_provider_identity(),
            false,
            PathBuf::from("/work"),
            &[(crate::types::Role::System, "fresh system".to_string())],
            HashMap::new(),
            test_tool_context(),
            Vec::new(),
            Vec::new(),
            None,
            uuid::Uuid::new_v4(),
        )
        .expect("fork should succeed");

        let Some(SessionPersistencePlan::Create { seed_records }) = run.persistence.as_ref() else {
            panic!("a fork persists a new session");
        };
        assert_fork_seed(seed_records, &run.agent.session_id().to_string());
        assert_eq!(
            run.agent
                .active_checkpoint()
                .expect("the fork keeps the latest checkpoint active")
                .session_id,
            run.agent.session_id().to_string(),
            "the in-memory checkpoint names the fork session too"
        );
        assert_eq!(
            projected_contents(&run.agent.request_context())
                .iter()
                .filter(|content| content.as_str() == "call:call-later")
                .count(),
            1,
            "the later exchange is projected once"
        );

        assert_eq!(
            projected_contents(&run.agent.request_context()),
            vec![
                "fresh system".to_string(),
                format!(
                    "{}\n\nnewer summary",
                    crate::types::compaction::HISTORICAL_SUMMARY_MARKER
                ),
                "call:call-1".to_string(),
                "output:call-1".to_string(),
                "new request".to_string(),
                "call:call-later".to_string(),
                "output:call-later".to_string(),
            ],
            "the fork projects the latest checkpoint, not retired raw history"
        );
    }

    #[test]
    fn checkpoint_free_fork_seeds_exactly_the_conversation_records() {
        let mut source = Session::new(uuid::Uuid::new_v4(), PathBuf::from("/work"));
        source.records = vec![
            message_record(crate::types::Role::User, "hello"),
            message_record(crate::types::Role::Assistant, "hi"),
            SessionRecord::TurnUsage(crate::types::TurnUsageData {
                session_id: source.id.to_string(),
                task_id: uuid::Uuid::new_v4().to_string(),
                turn: 1,
                usage: crate::types::Usage::default(),
                timestamp: chrono::Utc::now(),
                attempt: None,
                terminal_class: None,
                usage_presence: None,
                model: None,
                response_model: None,
                provider_request_id: None,
                context_window: None,
                remaining_context_tokens: None,
            }),
        ];

        let run = crate::CodingAssistant::forked_client_and_session(
            &source,
            test_resolved_model_config(),
            "test".to_string(),
            test_provider_identity(),
            false,
            PathBuf::from("/work"),
            &[(crate::types::Role::System, "fresh system".to_string())],
            HashMap::new(),
            test_tool_context(),
            Vec::new(),
            Vec::new(),
            None,
            uuid::Uuid::new_v4(),
        )
        .expect("fork should succeed");

        let Some(SessionPersistencePlan::Create { seed_records }) = run.persistence.as_ref() else {
            panic!("a fork persists a new session");
        };
        assert_eq!(
            seed_records.len(),
            2,
            "only conversation records are copied"
        );
        assert!(
            !seed_records
                .iter()
                .any(|record| matches!(record, SessionRecord::Compaction(_))),
            "a checkpoint-free fork gains no checkpoint"
        );
    }
}
