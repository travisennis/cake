//! Session construction from CLI arguments.
//!
//! Provides the free function [`skill_locations`] and
//! `impl CodingAssistant` methods that build agent/session pairs
//! for new, restored, and forked runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::info;

use crate::cli::run_mode::{RunMode, SessionStorage};
use crate::clients::{Agent, ToolContext};
use crate::config::skills::Skill;
use crate::config::{
    AgentsFile, DataDir, ModelDefinition, ResolvedModelConfig, Session, SkillCatalog,
};
use crate::prompts::build_initial_prompt_messages;
use crate::types::SessionRecord;

/// A fully assembled agent, session, and storage strategy ready for execution.
pub struct RunSession {
    pub(crate) agent: Agent,
    pub(crate) session: Session,
    pub(crate) storage: SessionStorage,
    pub(crate) seed_records: Option<Vec<SessionRecord>>,
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

impl crate::CodingAssistant {
    /// Convert a restored session into the agent/session pair used for a continued run.
    pub(crate) fn restored_client_and_session(
        restored: Session,
        resolved: ResolvedModelConfig,
        initial_messages: &[(crate::types::Role, String)],
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
    pub(crate) fn new_client_and_session(
        resolved: ResolvedModelConfig,
        current_dir: PathBuf,
        initial_messages: &[(crate::types::Role, String)],
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
    pub(crate) fn forked_client_and_session(
        restored: &Session,
        resolved: ResolvedModelConfig,
        current_dir: PathBuf,
        initial_messages: &[(crate::types::Role, String)],
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
        task_id: uuid::Uuid,
    ) -> anyhow::Result<RunSession> {
        let initial_messages =
            build_initial_prompt_messages(&current_dir, config_dir, agents_files, skill_catalog);
        let locs = skill_locations(skill_catalog);

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
                    &locs,
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
                    &locs,
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
                    locs,
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
                    locs,
                    Arc::clone(tool_context),
                    task_id,
                ))
            },
        }
    }
}
