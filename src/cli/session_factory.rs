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
use crate::clients::judge::{JudgeContext, judge_is_enabled};
use crate::clients::{Agent, ToolContext};
use crate::config::settings::{JUDGE_BYPASS_ENV, JudgeSettings};
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
        model_config_name: String,
        initial_messages: &[(crate::types::Role, String)],
        skill_locations: &HashMap<PathBuf, Skill>,
        tool_context: Arc<ToolContext>,
        toolbox_tools: Vec<ToolboxTool>,
        enabled_tools: Option<&[String]>,
        task_id: uuid::Uuid,
    ) -> anyhow::Result<RunSession> {
        let seed = restored.context_seed();
        // Seed once-per-session activation dedup from the persisted records so
        // a resumed run does not re-emit "first observed" for known skills.
        let activated_skills = restored.activated_skills();

        let agent = Agent::new(resolved.clone(), initial_messages)
            .with_session_id(restored.id)
            .with_task_id(task_id)
            .with_model_config_name(model_config_name.clone())
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_enabled_tools(enabled_tools)
            // Flattened rather than layered: the CLI prints only the outermost
            // error, and the underlying diagnostic is the useful part.
            .with_context_seed(seed)
            .map_err(|error| anyhow::anyhow!("Cannot restore session {}: {error:#}", restored.id))?
            .with_last_usage(restored.last_turn_usage())
            .with_skill_locations(skill_locations.clone())
            .with_activated_skills(activated_skills);
        let mut session = Session::new(restored.id, restored.working_dir);
        session.model = Some(resolved.model_config.model);
        session.model_config = Some(model_config_name);
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
        model_config_name: String,
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
            .with_model_config_name(model_config_name.clone())
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_enabled_tools(enabled_tools)
            .with_skill_locations(skill_locations);
        let new_id = agent.session_id();
        info!(target: "cake", "New session: {new_id}");
        let mut session = Session::new(new_id, current_dir);
        session.model = Some(resolved.model_config.model);
        session.model_config = Some(model_config_name);
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
        model_config_name: String,
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
            .with_model_config_name(model_config_name.clone())
            .with_tool_context(tool_context)
            .with_toolbox_tools(toolbox_tools)
            .with_enabled_tools(enabled_tools);
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
            tool_context,
            toolbox_tools,
            tools_enabled: enabled_tools,
            include_runtime_context: !self.no_runtime_context,
            task_id,
            judge,
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

    /// Resolve a restored session's model, attach the judge, rebuild the pair.
    fn restored_run(
        &self,
        restored: Session,
        inputs: &RunInputs<'_>,
    ) -> anyhow::Result<RunSession> {
        let (resolved, model_config_name) = self.resolve_model_for_session(
            inputs.models,
            inputs.default_model,
            restored.model.as_deref(),
            restored.model_config.as_deref(),
        )?;
        let initial_messages = inputs.initial_messages(resolved.model_config.supports_images);
        let tool_context =
            attach_judge(inputs.tool_context, &resolved, inputs.judge, inputs.models);
        Self::restored_client_and_session(
            restored,
            resolved,
            model_config_name,
            &initial_messages,
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
        let (resolved, model_config_name) = self.resolve_model_for_session(
            inputs.models,
            inputs.default_model,
            restored.model.as_deref(),
            restored.model_config.as_deref(),
        )?;
        let initial_messages = inputs.initial_messages(resolved.model_config.supports_images);
        let tool_context =
            attach_judge(inputs.tool_context, &resolved, inputs.judge, inputs.models);
        Self::forked_client_and_session(
            &restored,
            resolved,
            model_config_name,
            inputs.current_dir.clone(),
            &initial_messages,
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
        let (config, model_config_name) =
            self.resolve_model_config(inputs.models, inputs.default_model)?;
        let resolved = ResolvedModelConfig::resolve(config)?;
        let initial_messages = inputs.initial_messages(resolved.model_config.supports_images);
        Ok(Self::new_client_and_session(
            resolved.clone(),
            model_config_name,
            inputs.current_dir.clone(),
            &initial_messages,
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
    config_dir: &'a Path,
    cli_system_prompt: Option<&'a Path>,
    settings_system_prompt: Option<&'a Path>,
    agents_files: &'a [AgentsFile],
    skill_catalog: &'a SkillCatalog,
    skill_locations: HashMap<PathBuf, Skill>,
    models: &'a HashMap<String, ModelDefinition>,
    default_model: Option<&'a str>,
    tool_context: &'a Arc<ToolContext>,
    toolbox_tools: &'a [ToolboxTool],
    tools_enabled: Option<&'a [String]>,
    include_runtime_context: bool,
    task_id: uuid::Uuid,
    judge: &'a JudgeSettings,
}

impl RunInputs<'_> {
    /// Build the initial prompt messages for a resolved model.
    ///
    /// The tool list is built after the model is resolved so it can reflect
    /// model capabilities: a model that does not accept image input never
    /// advertises `ReadImage`.
    fn initial_messages(&self, supports_images: bool) -> Vec<(crate::types::Role, String)> {
        build_initial_prompt_messages_with_enabled_tools(
            &self.current_dir,
            self.config_dir,
            self.cli_system_prompt,
            self.settings_system_prompt,
            self.agents_files,
            self.skill_catalog,
            self.tool_context.sandbox_policy,
            self.toolbox_tools,
            self.tools_enabled,
            supports_images,
            self.include_runtime_context,
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

/// Attach the LLM-judge context to the tool context shared by the Bash
/// preflight.
///
/// The judge defaults to the agent's resolved model; a `[tools.bash.judge]
/// model` override is resolved lazily at call time (after the bypass check) so
/// a broken judge config cannot defeat the emergency bypass.
///
/// The `CAKE_JUDGE` emergency bypass is read here, once, when the run's context
/// is built, rather than on every Bash call. The value is carried on the
/// context so the Bash preflight never consults the process environment and
/// tests can supply the value directly.
fn attach_judge(
    tool_context: &Arc<ToolContext>,
    agent_model: &ResolvedModelConfig,
    judge: &JudgeSettings,
    models: &HashMap<String, ModelDefinition>,
) -> Arc<ToolContext> {
    let bypass_env = std::env::var(JUDGE_BYPASS_ENV).ok();
    let gates = if judge_is_enabled(judge, bypass_env.as_deref()) {
        let context = JudgeContext {
            settings: judge.clone(),
            bypass_env,
            agent_model: agent_model.clone(),
            models: models.clone(),
            client: std::sync::OnceLock::new(),
            record_attempt: None,
        };
        crate::gates::judge_gate::builtin_pipeline(std::sync::Arc::new(context))
            .map(std::sync::Arc::new)
            .map_err(|error| {
                tracing::warn!(
                    target: "cake",
                    "Failed to start the built-in safety gate: {error}"
                );
                error
            })
            .ok()
    } else {
        Some(std::sync::Arc::new(
            crate::gates::composer::GatePipeline::bypass(),
        ))
    };
    Arc::new((**tool_context).clone().with_gates(gates))
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
            "test".to_string(),
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
            "test".to_string(),
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
            PathBuf::from("/work"),
            &[(crate::types::Role::System, "fresh system".to_string())],
            HashMap::new(),
            test_tool_context(),
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
            PathBuf::from("/work"),
            &[(crate::types::Role::System, "fresh system".to_string())],
            HashMap::new(),
            test_tool_context(),
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
