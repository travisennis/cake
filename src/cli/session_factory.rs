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
use crate::clients::judge::JudgeContext;
use crate::clients::{Agent, ToolContext};
use crate::config::settings::JudgeSettings;
use crate::config::skills::Skill;
use crate::config::toolbox::ToolboxTool;
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

impl crate::CodingAssistant {
    /// Convert a restored session into the agent/session pair used for a continued run.
    pub(crate) fn restored_client_and_session(
        restored: Session,
        resolved: ResolvedModelConfig,
        initial_messages: &[(crate::types::Role, String)],
        skill_locations: &HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        toolbox_tools: Vec<ToolboxTool>,
        task_id: uuid::Uuid,
    ) -> anyhow::Result<RunSession> {
        let messages = restored.messages();

        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_session_id(restored.id)
            .with_task_id(task_id)
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            // Flattened rather than layered: the CLI prints only the outermost
            // error, and the underlying diagnostic is the useful part.
            .with_history(messages)
            .map_err(|error| anyhow::anyhow!("Cannot restore session {}: {error:#}", restored.id))?
            .with_skill_locations(skill_locations.clone());
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
        toolbox_tools: Vec<ToolboxTool>,
        task_id: uuid::Uuid,
    ) -> RunSession {
        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_task_id(task_id)
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
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
    #[expect(
        clippy::too_many_arguments,
        reason = "session construction naturally requires many parameters"
    )]
    pub(crate) fn forked_client_and_session(
        restored: &Session,
        resolved: ResolvedModelConfig,
        current_dir: PathBuf,
        initial_messages: &[(crate::types::Role, String)],
        skill_locations: HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        toolbox_tools: Vec<ToolboxTool>,
        task_id: uuid::Uuid,
    ) -> anyhow::Result<RunSession> {
        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_task_id(task_id)
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_history(restored.messages())
            .map_err(|error| anyhow::anyhow!("Cannot fork session {}: {error:#}", restored.id))?
            .with_skill_locations(skill_locations);
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
    #[expect(
        clippy::too_many_lines,
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
        task_id: uuid::Uuid,
        loaded_system_prompt: Option<&str>,
        judge: &JudgeSettings,
    ) -> anyhow::Result<RunSession> {
        let cli_system_prompt = self.system_prompt.as_deref().map(std::path::Path::new);
        let settings_system_prompt = loaded_system_prompt.map(std::path::Path::new);
        let initial_messages = build_initial_prompt_messages(
            &current_dir,
            config_dir,
            cli_system_prompt,
            settings_system_prompt,
            agents_files,
            skill_catalog,
            tool_context.sandbox_policy,
            toolbox_tools,
        );
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
                let tool_context = attach_judge(tool_context, &resolved, judge, models);
                Self::restored_client_and_session(
                    restored,
                    resolved,
                    &initial_messages,
                    &locs,
                    tool_context,
                    toolbox_tools.to_vec(),
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
                let tool_context = attach_judge(tool_context, &resolved, judge, models);
                Self::restored_client_and_session(
                    restored,
                    resolved,
                    &initial_messages,
                    &locs,
                    tool_context,
                    toolbox_tools.to_vec(),
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
                let tool_context = attach_judge(tool_context, &resolved, judge, models);
                Self::forked_client_and_session(
                    &restored,
                    resolved,
                    current_dir,
                    &initial_messages,
                    locs,
                    tool_context,
                    toolbox_tools.to_vec(),
                    task_id,
                )
            },
            RunMode::NewSession | RunMode::Ephemeral => {
                let resolved = ResolvedModelConfig::resolve(
                    self.resolve_model_config(models, default_model)?,
                )?;
                Ok(Self::new_client_and_session(
                    resolved.clone(),
                    current_dir,
                    &initial_messages,
                    locs,
                    attach_judge(tool_context, &resolved, judge, models),
                    toolbox_tools.to_vec(),
                    task_id,
                ))
            },
        }
    }
}

/// Attach the LLM-judge context to the tool context shared by the Bash
/// preflight.
///
/// The judge defaults to the agent's resolved model; a `[tools.bash.judge]
/// model` override is resolved lazily at call time (after the bypass check) so
/// a broken judge config cannot defeat the emergency bypass.
fn attach_judge(
    tool_context: &Arc<ToolContext>,
    agent_model: &ResolvedModelConfig,
    judge: &JudgeSettings,
    models: &HashMap<String, ModelDefinition>,
) -> Arc<ToolContext> {
    let context = JudgeContext {
        settings: judge.clone(),
        agent_model: agent_model.clone(),
        models: models.clone(),
        client: std::sync::OnceLock::new(),
        record_attempt: None,
    };
    Arc::new((**tool_context).clone().with_judge(Some(Arc::new(context))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SandboxPolicy;
    use crate::config::settings::ModelDefinition;
    use crate::config::skills::SkillCatalog;
    use clap::Parser;

    /// A session built through `build_client_and_session` must carry the
    /// command-safety judge context on the agent's tool context.
    ///
    /// The e2e session-mode tests (`tests/session_modes.rs`) never invoke
    /// Bash, so they cannot observe this wiring; this unit test closes that
    /// gap (review F-003): without `attach_judge`, every Bash call in a real
    /// session fails closed.
    #[test]
    fn new_session_carries_judge_on_tool_context() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let cli = crate::CodingAssistant::parse_from(["cake"]);
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");
            let config_dir = tempfile::tempdir().expect("temp config dir");
            let models = HashMap::from([(
                "test".to_string(),
                ModelDefinition {
                    name: "test".to_string(),
                    model: "glm-5.1".to_string(),
                    base_url: "https://example.invalid/v1".to_string(),
                    api_key_env: "SESSION_FACTORY_TEST_KEY".to_string(),
                    provider: None,
                    provider_headers: None,
                    api_type: crate::config::model::ApiType::Responses,
                    temperature: None,
                    top_p: None,
                    max_output_tokens: None,
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
                    uuid::Uuid::new_v4(),
                    None,
                    &JudgeSettings::default(),
                )
                .expect("session construction should succeed");

            assert!(
                run_session.agent.tool_context().judge.is_some(),
                "the run's tool context must carry the command-safety judge"
            );
        });
    }
}
