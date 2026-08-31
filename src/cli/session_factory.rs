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
use crate::clients::judge::JudgeContext;
use crate::clients::{Agent, ToolContext};
use crate::config::settings::JudgeSettings;
use crate::config::skills::Skill;
use crate::config::toolbox::ToolboxTool;
use crate::config::{
    AgentsFile, DataDir, ModelDefinition, ResolvedModelConfig, Session, SkillCatalog,
};
use crate::prompts::build_initial_prompt_messages_with_enabled_tools;
use crate::types::SessionRecord;

/// A fully assembled agent, session, and persistence plan ready for execution.
pub struct RunSession {
    pub(crate) agent: Agent,
    pub(crate) session: Session,
    pub(crate) persistence: Option<SessionPersistencePlan>,
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
    #[expect(
        clippy::too_many_arguments,
        reason = "session construction naturally requires many parameters"
    )]
    pub(crate) fn restored_client_and_session(
        restored: Session,
        resolved: ResolvedModelConfig,
        initial_messages: &[(crate::types::Role, String)],
        skill_locations: &HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        toolbox_tools: Vec<ToolboxTool>,
        enabled_tools: Option<&[String]>,
        task_id: uuid::Uuid,
    ) -> anyhow::Result<RunSession> {
        let messages = restored.messages();
        // Seed once-per-session activation dedup from the persisted records so
        // a resumed run does not re-emit "first observed" for known skills.
        let activated_skills = restored.activated_skills();

        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_session_id(restored.id)
            .with_task_id(task_id)
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_enabled_tools(enabled_tools)
            // Flattened rather than layered: the CLI prints only the outermost
            // error, and the underlying diagnostic is the useful part.
            .with_history(messages)
            .map_err(|error| anyhow::anyhow!("Cannot restore session {}: {error:#}", restored.id))?
            .with_last_usage(restored.last_turn_usage())
            .with_skill_locations(skill_locations.clone())
            .with_activated_skills(activated_skills);
        let mut session = Session::new(restored.id, restored.working_dir);
        session.model = Some(resolved.model_config.model);
        Ok(RunSession {
            agent,
            session,
            persistence: Some(SessionPersistencePlan::Append),
        })
    }

    /// Build the agent/session pair for a new run using the agent-generated session id.
    #[expect(
        clippy::too_many_arguments,
        reason = "session construction naturally requires many parameters"
    )]
    pub(crate) fn new_client_and_session(
        resolved: ResolvedModelConfig,
        current_dir: PathBuf,
        initial_messages: &[(crate::types::Role, String)],
        skill_locations: HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        toolbox_tools: Vec<ToolboxTool>,
        enabled_tools: Option<&[String]>,
        task_id: uuid::Uuid,
        persistence: Option<SessionPersistencePlan>,
    ) -> RunSession {
        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_task_id(task_id)
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_enabled_tools(enabled_tools)
            .with_skill_locations(skill_locations);
        let new_id = agent.session_id();
        info!(target: "cake", "New session: {new_id}");
        let mut session = Session::new(new_id, current_dir);
        session.model = Some(resolved.model_config.model);
        session.system_prompt = initial_messages.first().map(|(_, content)| content.clone());
        RunSession {
            agent,
            session,
            persistence,
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
        enabled_tools: Option<&[String]>,
        task_id: uuid::Uuid,
    ) -> anyhow::Result<RunSession> {
        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_task_id(task_id)
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_enabled_tools(enabled_tools)
            .with_history(restored.messages())
            .map_err(|error| anyhow::anyhow!("Cannot fork session {}: {error:#}", restored.id))?
            .with_last_usage(restored.last_turn_usage())
            .with_skill_locations(skill_locations)
            // The fork's session file starts with copies of the source
            // session's SkillActivated records; seed the same names so re-reads
            // do not emit duplicates.
            .with_activated_skills(restored.activated_skills());
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
            persistence: Some(SessionPersistencePlan::Create { seed_records }),
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
        enabled_tools: Option<&[String]>,
        task_id: uuid::Uuid,
        loaded_system_prompt: Option<&str>,
        judge: &JudgeSettings,
    ) -> anyhow::Result<RunSession> {
        let initial_messages = build_initial_prompt_messages_with_enabled_tools(
            &current_dir,
            config_dir,
            self.system_prompt.as_deref().map(std::path::Path::new),
            loaded_system_prompt.map(std::path::Path::new),
            agents_files,
            skill_catalog,
            tool_context.sandbox_policy,
            toolbox_tools,
            enabled_tools,
        );
        let inputs = RunInputs {
            current_dir,
            initial_messages,
            skill_locations: skill_locations(skill_catalog),
            models,
            default_model,
            tool_context,
            toolbox_tools,
            tools_enabled: enabled_tools,
            task_id,
            judge,
        };
        match run_mode {
            RunMode::ContinueLatest => self.continue_latest_run(data_dir, &inputs),
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

    /// `--continue`: restore the latest session recorded for this directory.
    fn continue_latest_run(
        &self,
        data_dir: &DataDir,
        inputs: &RunInputs<'_>,
    ) -> anyhow::Result<RunSession> {
        info!(
            target: "cake",
            "Continuing latest session for directory: {}",
            inputs.current_dir.display()
        );
        let Some(restored) = data_dir.load_latest_session(&inputs.current_dir)? else {
            return Err(missing_continue_target(data_dir, &inputs.current_dir)?);
        };
        info!(target: "cake", "Continuing session: {}", restored.id);
        self.restored_run(restored, inputs)
    }

    /// Resolve a restored session's model, attach the judge, rebuild the pair.
    fn restored_run(
        &self,
        restored: Session,
        inputs: &RunInputs<'_>,
    ) -> anyhow::Result<RunSession> {
        let resolved = self.resolve_model_for_session(
            inputs.models,
            inputs.default_model,
            restored.model.as_deref(),
        )?;
        let tool_context =
            attach_judge(inputs.tool_context, &resolved, inputs.judge, inputs.models);
        Self::restored_client_and_session(
            restored,
            resolved,
            &inputs.initial_messages,
            &inputs.skill_locations,
            tool_context,
            inputs.toolbox_tools.to_vec(),
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
        let resolved = self.resolve_model_for_session(
            inputs.models,
            inputs.default_model,
            restored.model.as_deref(),
        )?;
        let tool_context =
            attach_judge(inputs.tool_context, &resolved, inputs.judge, inputs.models);
        Self::forked_client_and_session(
            &restored,
            resolved,
            inputs.current_dir.clone(),
            &inputs.initial_messages,
            inputs.skill_locations.clone(),
            tool_context,
            inputs.toolbox_tools.to_vec(),
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
        let resolved = ResolvedModelConfig::resolve(
            self.resolve_model_config(inputs.models, inputs.default_model)?,
        )?;
        Ok(Self::new_client_and_session(
            resolved.clone(),
            inputs.current_dir.clone(),
            &inputs.initial_messages,
            inputs.skill_locations.clone(),
            attach_judge(inputs.tool_context, &resolved, inputs.judge, inputs.models),
            inputs.toolbox_tools.to_vec(),
            inputs.tools_enabled,
            inputs.task_id,
            persistence,
        ))
    }
}

/// Construction inputs every run-mode step shares.
struct RunInputs<'a> {
    current_dir: PathBuf,
    initial_messages: Vec<(crate::types::Role, String)>,
    skill_locations: HashMap<PathBuf, Skill>,
    models: &'a HashMap<String, ModelDefinition>,
    default_model: Option<&'a str>,
    tool_context: &'a Arc<ToolContext>,
    toolbox_tools: &'a [ToolboxTool],
    tools_enabled: Option<&'a [String]>,
    task_id: uuid::Uuid,
    judge: &'a JudgeSettings,
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

/// Explain why `--continue` has nothing to restore.
///
/// Names the directory that owns the most recent session when one exists;
/// lookup failures propagate rather than being masked by this error.
fn missing_continue_target(
    data_dir: &DataDir,
    current_dir: &Path,
) -> anyhow::Result<anyhow::Error> {
    let Some(latest) = data_dir.load_latest_session_any_directory()? else {
        return Ok(anyhow::anyhow!(
            "No previous session found for this directory"
        ));
    };
    Ok(anyhow::anyhow!(
        "Cannot continue: latest session was created in '{}' but current directory is '{}'. \
         Run from the original directory or start a new session.",
        latest.working_dir.display(),
        current_dir.display()
    ))
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
    use std::collections::HashSet;

    fn test_resolved_model_config() -> ResolvedModelConfig {
        ResolvedModelConfig {
            model_config: crate::config::model::ModelConfig {
                model: "test".to_string(),
                api_type: crate::config::model::ApiType::ChatCompletions,
                base_url: "https://example.invalid/v1".to_string(),
                api_key_env: "SESSION_FACTORY_TEST_KEY".to_string(),
                provider: None,
                provider_headers: None,
                temperature: None,
                top_p: None,
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
                    None,
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
                },
            ));

        let resolved = ResolvedModelConfig {
            model_config: crate::config::model::ModelConfig {
                model: "test".to_string(),
                api_type: crate::config::model::ApiType::ChatCompletions,
                base_url: "https://example.invalid/v1".to_string(),
                api_key_env: "SESSION_FACTORY_TEST_KEY".to_string(),
                provider: None,
                provider_headers: None,
                temperature: None,
                top_p: None,
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
            &[(crate::types::Role::System, "test".to_string())],
            &HashMap::new(),
            tool_context,
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
            &[(crate::types::Role::System, "test".to_string())],
            &HashMap::new(),
            test_tool_context(),
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
            PathBuf::from("/work"),
            &[(crate::types::Role::System, "test".to_string())],
            HashMap::new(),
            test_tool_context(),
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

    // ---- build_client_and_session run-mode coverage ----

    fn test_models() -> HashMap<String, ModelDefinition> {
        HashMap::from([(
            "test".to_string(),
            ModelDefinition {
                name: "test".to_string(),
                model: "glm-5.1".to_string(),
                base_url: "https://example.invalid/v1".to_string(),
                api_key_env: "SESSION_FACTORY_TEST_KEY".to_string(),
                provider: None,
                provider_headers: None,
                api_type: crate::config::model::ApiType::ChatCompletions,
                temperature: None,
                top_p: None,
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
        session.model = Some("test".to_string());
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
            None,
            uuid::Uuid::new_v4(),
            None,
            &JudgeSettings::default(),
        )
    }

    #[test]
    fn continue_restores_latest_directory_session() {
        temp_env::with_var("SESSION_FACTORY_TEST_KEY", Some("test-key"), || {
            let cli = crate::CodingAssistant::parse_from(["cake"]);
            let data_dir_dir = tempfile::tempdir().expect("temp data dir");
            let data_dir = DataDir::new_in_dir(data_dir_dir.path());
            let working_dir = tempfile::tempdir().expect("temp working dir");
            let saved = saved_session(&data_dir, working_dir.path());

            let run = build_for_mode(
                &cli,
                &RunMode::ContinueLatest,
                &data_dir,
                working_dir.path(),
            )
            .expect("continue should succeed");

            assert_eq!(run.agent.session_id(), saved.id);
            assert!(matches!(
                run.persistence,
                Some(SessionPersistencePlan::Append)
            ));
        });
    }

    #[test]
    fn continue_without_any_sessions_reports_missing_session() {
        let cli = crate::CodingAssistant::parse_from(["cake"]);
        let data_dir_dir = tempfile::tempdir().expect("temp data dir");
        let data_dir = DataDir::new_in_dir(data_dir_dir.path());
        let working_dir = tempfile::tempdir().expect("temp working dir");

        let error = build_for_mode(
            &cli,
            &RunMode::ContinueLatest,
            &data_dir,
            working_dir.path(),
        )
        .err()
        .expect("continue without sessions should fail");

        assert!(error.to_string().contains("No previous session found"));
    }

    #[test]
    fn continue_in_other_directory_names_the_original_directory() {
        let cli = crate::CodingAssistant::parse_from(["cake"]);
        let data_dir_dir = tempfile::tempdir().expect("temp data dir");
        let data_dir = DataDir::new_in_dir(data_dir_dir.path());
        let working_dir = tempfile::tempdir().expect("temp working dir");
        let other_dir = tempfile::tempdir().expect("temp other dir");
        saved_session(&data_dir, other_dir.path());

        let error = build_for_mode(
            &cli,
            &RunMode::ContinueLatest,
            &data_dir,
            working_dir.path(),
        )
        .err()
        .expect("continue from another directory should fail");

        let message = error.to_string();
        assert!(message.contains("Cannot continue"));
        assert!(message.contains(&other_dir.path().display().to_string()));
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
            saved.model = Some("test".to_string());
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
}
