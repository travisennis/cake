mod agent_loop;
mod agent_telemetry;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::OutputFormat;
use crate::clients::agent_observer::AgentObserver;
use crate::clients::agent_runner::AgentRunner;
use crate::clients::agent_state::{ConversationState, accumulate_usage};
use crate::clients::backend::Backend;
use crate::clients::mcp::{McpClient, McpServer, McpTool};
use crate::clients::tools::{
    SKILL_SEARCH_NAME, SandboxPolicy, SkillSearchEntry, TOOL_SEARCH_NAME, ToolContext,
    ToolRegistry, ToolSearchEntry, default_tool_registry, mcp_tool_entry, skill_search_enabled,
    skill_search_entry, tool_search_enabled, tool_search_entry, toolbox_tool_entry,
};
use crate::config::model::ResolvedModelConfig;
use crate::config::output_schema::OutputSchema;
use crate::config::settings::ResolvedLimits;
use crate::config::skills::Skill;
use crate::config::toolbox::ToolboxTool;
use crate::hooks::HookRunner;
use crate::session_telemetry::{
    GateTelemetrySink, ProviderTermination, SessionTelemetryContext, SessionTelemetryRecord,
    SessionTelemetrySettings, SessionTelemetryWriter, SharedSessionTelemetryWriter,
};
use crate::types::{
    ApiAttemptTerminalClass, ConversationItem, ReplaySafety, ReportedUsage, Role, SessionRecord,
    StreamRecord, TaskCompleteData, TaskOutcome, TaskStartData, TurnUsageData, TurnUsagePurpose,
    Usage, UsagePresence,
};

/// Result of a single API turn (one request/response cycle).
#[derive(Debug)]
pub(super) struct TurnResult {
    pub(super) items: Vec<ConversationItem>,
    pub(super) usage: Option<ReportedUsage>,
    pub(super) termination: Option<ProviderTermination>,
    pub(super) provider_request_id: Option<String>,
    /// The model the provider reported serving the response, when the wire
    /// format supplied one. May differ from the configured model when a
    /// gateway routes an alias.
    pub(super) response_model: Option<String>,
}

/// One provider attempt whose reported usage must be settled into the
/// session ledger before the runner classifies or retries the attempt.
///
/// Carries the bounded provider identity of that attempt — the configured
/// model, the provider response ID, and the response-reported model when the
/// provider supplied one — so a usage record can be tied to the provider
/// response that produced it (ADR-030).
#[derive(Debug, Clone)]
pub(super) struct TurnUsageSettlement {
    pub(super) turn_index: u32,
    pub(super) attempt: u32,
    pub(super) terminal_class: ApiAttemptTerminalClass,
    pub(super) usage: ReportedUsage,
    pub(super) model: String,
    pub(super) provider_request_id: Option<String>,
    pub(super) response_model: Option<String>,
    pub(super) purpose: Option<TurnUsagePurpose>,
    pub(super) compaction_id: Option<String>,
    pub(super) summary_step: Option<u32>,
}

/// Purpose metadata attached to every attempt of one compaction summary
/// request. `None` on the runner call means a main-agent attempt.
#[derive(Debug, Clone, Default)]
pub(super) struct AttemptPurpose {
    pub(super) purpose: Option<TurnUsagePurpose>,
    pub(super) compaction_id: Option<String>,
    pub(super) summary_step: Option<u32>,
}

impl AttemptPurpose {
    pub(super) fn compaction(checkpoint_id: &str, summary_step: u32) -> Self {
        Self {
            purpose: Some(TurnUsagePurpose::Compaction),
            compaction_id: Some(checkpoint_id.to_string()),
            summary_step: Some(summary_step),
        }
    }
}

pub(super) fn record_turn_usage(
    observer: &mut AgentObserver,
    total_usage: &mut Usage,
    last_usage: &mut Option<Usage>,
    session_id: uuid::Uuid,
    task_id: uuid::Uuid,
    context_window: Option<u32>,
    settlement: TurnUsageSettlement,
) {
    let TurnUsageSettlement {
        turn_index,
        attempt,
        terminal_class,
        usage,
        model,
        provider_request_id,
        response_model,
        purpose,
        compaction_id,
        summary_step,
    } = settlement;
    accumulate_usage(total_usage, Some(&usage.usage));
    let is_compaction = matches!(purpose, Some(TurnUsagePurpose::Compaction));
    if !is_compaction {
        *last_usage = Some(usage.usage);
    }

    // A context fraction is only meaningful when the provider reported every
    // counter and the model configures a window. A partial report would
    // otherwise read as a measured context size the provider never gave us.
    // Summary attempts never describe the main-agent context.
    let budget = match context_window {
        Some(window) if !is_compaction && usage.presence == UsagePresence::Complete => Some((
            window,
            u64::from(window).saturating_sub(usage.usage.input_tokens),
        )),
        _ => None,
    };

    let legacy_shape =
        !is_compaction && attempt == 1 && terminal_class == ApiAttemptTerminalClass::Completed;
    let data = TurnUsageData {
        session_id: session_id.to_string(),
        task_id: task_id.to_string(),
        turn: turn_index,
        usage: usage.usage,
        timestamp: chrono::Utc::now(),
        attempt: (!legacy_shape).then_some(attempt),
        terminal_class: (!legacy_shape).then_some(terminal_class),
        usage_presence: Some(usage.presence),
        model: Some(model),
        response_model,
        provider_request_id,
        context_window: budget.map(|(window, _)| window),
        remaining_context_tokens: budget.map(|(_, remaining)| remaining),
        purpose,
        compaction_id,
        summary_step,
    };
    if let Err(error) = observer.persist_record(&SessionRecord::TurnUsage(data.clone())) {
        tracing::warn!(target: "cake", %error, "Failed to persist per-attempt usage");
    }
    if !is_compaction {
        observer.stream_audit_record(&StreamRecord::TurnUsage(data));
    }
}

struct AgentTelemetry {
    context: SessionTelemetryContext,
    writer: Arc<SharedSessionTelemetryWriter>,
}

/// One MCP tool kept out of the registry until `ToolSearch` selects it.
#[derive(Clone)]
struct DeferredMcpTool {
    server: String,
    tool: McpTool,
    client: Arc<McpClient>,
}

// =============================================================================
// Agent (shared loop over any backend)
// =============================================================================

/// Orchestrates conversation loops, tool execution, and API communication.
///
/// The `Agent` manages the conversation history, executes tool calls from the
/// AI model, and handles streaming output. It supports multiple API backends
/// through the `ApiType` configuration.
pub struct Agent {
    config: ResolvedModelConfig,
    runner: AgentRunner,
    observer: AgentObserver,
    /// Conversation history using typed items
    conversation: ConversationState,
    tools: ToolRegistry,
    /// Optional exact-name selection applied to tools added after the initial
    /// registry is filtered.
    enabled_tool_names: Option<Vec<String>>,
    /// MCP tools that are searchable but not registered yet.
    deferred_mcp_tools: Vec<DeferredMcpTool>,
    /// Connected MCP servers and their latest advertised tool snapshots.
    mcp_servers: Vec<McpServer>,
    /// Last tool-change generation observed for each connected server.
    mcp_tool_generations: HashMap<String, u64>,
    /// Registered names selected by `ToolSearch` during the current tool batch.
    pending_tool_activations: Arc<Mutex<Vec<String>>>,
    /// Deferred tools already added to the registry. Shared with `ToolSearch`
    /// so a later search can distinguish active tools from new matches.
    active_mcp_tools: Arc<Mutex<HashSet<String>>>,
    /// Whether this agent installed the `ToolSearch` entry.
    tool_search_installed: bool,
    /// Whether this agent installed the `SkillSearch` entry.
    skill_search_installed: bool,
    tool_context: Arc<ToolContext>,
    /// Session ID for tracking
    session_id: uuid::Uuid,
    /// Task ID for the current CLI invocation.
    task_id: uuid::Uuid,
    /// The `[[models]]` entry name this run resolved to, reported on the
    /// `task_start` record. `None` when no entry name was resolved.
    model_config_name: Option<String>,
    /// Accumulated usage across all reported provider attempts
    total_usage: Usage,
    /// Usage reported by the most recent provider attempt. The basis for
    /// estimating remaining context-window budget. `None` before the first
    /// turn or when no turn has reported usage.
    last_usage: Option<Usage>,
    /// Number of API calls made
    turn_count: u32,
    /// Number of tool calls executed
    tool_call_count: u32,
    /// User-configured maximum number of agent-loop turns. `None` (the
    /// default) leaves the loop uncapped.
    max_turns: Option<u32>,
    /// User-configured maximum number of tool calls executed. `None` (the
    /// default) leaves the loop uncapped.
    max_tool_calls: Option<u32>,
    /// Maps SKILL.md paths to skills for path-watching `SkillActivated` records.
    /// When the Read tool targets one of these paths, a `SkillActivated`
    /// record is emitted once per skill per session.
    skill_locations: Arc<HashMap<PathBuf, Skill>>,
    /// Names of skills that have already had a `SkillActivated` record emitted
    /// in this session. Shared between concurrent tool executions.
    activated_skills: Arc<Mutex<HashSet<String>>>,
    /// Accumulated permission/policy denials encountered during this task.
    /// Hook-blocked tool calls are recorded here so the task completion
    /// record can report them as structured `permission_denials`.
    permission_denials: Vec<String>,
    /// Optional user-configured command hook runner.
    hook_runner: Option<Arc<HookRunner>>,
    /// Optional best-effort telemetry sidecar writer.
    telemetry: Option<AgentTelemetry>,
    /// Optional compiled JSON Schema constraining the final response
    /// (`--output-schema`). Intermediate turns run unconstrained.
    output_schema: Option<Arc<OutputSchema>>,
    /// Whether correction turns may attach the provider's native
    /// structured-output constraint. Cleared for the rest of the run when the
    /// provider rejects the constrained request (HTTP 400); never reset.
    native_constraint_enabled: bool,
}

impl Agent {
    /// Creates a new agent with the given configuration and initial prompt messages.
    ///
    /// The agent is initialized with the default built-in tools (Bash, `BashSession`,
    /// Read, Edit, and Write), plus `ReadImage` when the model declares image
    /// support. Attaching a read-only tool context via [`Self::with_tool_context`]
    /// retains only the read-safe tools. A new session ID is generated
    /// automatically.
    pub fn new(config: ResolvedModelConfig, initial_messages: &[(Role, String)]) -> Self {
        let mut tools = default_tool_registry();
        tools.retain_model_supported_tools(config.model_config.supports_images);
        Self {
            runner: AgentRunner::new(Backend::from_api_type(config.model_config.api_type)),
            config,
            observer: AgentObserver::default(),
            conversation: ConversationState::new(initial_messages),
            tools,
            enabled_tool_names: None,
            deferred_mcp_tools: Vec::new(),
            mcp_servers: Vec::new(),
            mcp_tool_generations: HashMap::new(),
            pending_tool_activations: Arc::new(Mutex::new(Vec::new())),
            active_mcp_tools: Arc::new(Mutex::new(HashSet::new())),
            tool_search_installed: false,
            skill_search_installed: false,
            tool_context: Arc::new(ToolContext::from_current_process()),
            session_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::new_v4(),
            model_config_name: None,
            total_usage: Usage::default(),
            last_usage: None,
            turn_count: 0,
            tool_call_count: 0,
            max_turns: None,
            max_tool_calls: None,
            skill_locations: Arc::new(HashMap::new()),
            activated_skills: Arc::new(Mutex::new(HashSet::new())),
            permission_denials: Vec::new(),
            hook_runner: None,
            telemetry: None,
            output_schema: None,
            native_constraint_enabled: true,
        }
    }

    /// Returns the enabled tool names.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.names()
    }

    /// Returns the tool context attached to this agent (for tests).
    #[cfg(test)]
    pub(crate) const fn tool_context(&self) -> &Arc<ToolContext> {
        &self.tool_context
    }

    #[cfg(test)]
    fn history(&self) -> &[ConversationItem] {
        self.conversation.history()
    }

    #[cfg(test)]
    const fn history_mut(&mut self) -> &mut Vec<ConversationItem> {
        self.conversation.history_mut()
    }

    /// Sets the directory context used for tool execution and sandboxing.
    ///
    /// Under the read-only sandbox policy, the Edit and Write tools are
    /// removed from the registry: they mutate files in-process, outside the
    /// OS sandbox that only wraps Bash, so they must not be offered at all
    /// for the policy's no-mutation guarantee to hold.
    pub fn with_tool_context(mut self, context: Arc<ToolContext>) -> Self {
        if context.sandbox_policy() == SandboxPolicy::ReadOnly {
            self.tools.retain_read_safe_tools();
        }
        self.tool_context = context;
        self
    }

    /// Initialize policy gates after resolving a new, resumed, or forked model.
    /// Failed startup removes both execution surfaces; an explicit bypass is
    /// the only way to expose Bash without a ready enforce gate.
    pub async fn with_policy_gates(
        mut self,
        loaded: &crate::config::settings::LoadedSettings,
        no_gate: bool,
    ) -> Self {
        let pipeline = crate::gates::runtime::start(
            &loaded.gates,
            &loaded.judge,
            &loaded.models,
            Some(&self.config.model_config),
            &self.tool_context.cwd,
            no_gate,
        )
        .await;
        let gates = match pipeline {
            Ok(pipeline) => Some(Arc::new(pipeline)),
            Err(error) => {
                eprintln!(
                    "Bash and BashSession disabled: {error}. Run cake gates doctor or configure cake-safety-judge."
                );
                self.tools.remove_bash_tools();
                None
            },
        };
        self.tool_context = Arc::new((*self.tool_context).clone().with_gates(gates));
        self
    }

    /// Registers user-defined toolbox tools (`tb__*`) alongside the built-in
    /// tools.
    ///
    /// Call after [`Self::with_session_id`], if any: each tool's executor
    /// captures the current session id to expose `CAKE_THREAD_ID` and
    /// `AGENT_THREAD_ID` to the tool process.
    ///
    /// Toolbox tools run as unsandboxed external processes, so under the
    /// read-only sandbox policy they are never registered — this method
    /// skips them when the current tool context is read-only, and
    /// [`Self::with_tool_context`] strips any already-registered `tb__*`
    /// entries when it applies a read-only context, keeping the policy's
    /// no-mutation guarantee regardless of builder call order.
    pub fn with_toolbox_tools(mut self, toolbox_tools: Vec<ToolboxTool>) -> Self {
        if self.tool_context.sandbox_policy() == SandboxPolicy::ReadOnly {
            return self;
        }
        let enabled_tool_names = self.enabled_tool_names.clone();
        for tool in toolbox_tools.into_iter().filter(move |tool| {
            enabled_tool_names
                .as_ref()
                .is_none_or(|enabled| enabled.iter().any(|name| name == &tool.registered_name))
        }) {
            self.tools
                .push_entry(toolbox_tool_entry(tool, self.session_id));
        }
        self
    }

    /// Registers tools discovered from MCP servers (`mcp__*`).
    ///
    /// MCP servers are trusted external processes, like toolbox tools, so they
    /// are never registered under the read-only sandbox policy. Each entry
    /// captures the server connection, so the process stays alive for the run
    /// and every tool call shares one JSON-RPC transport.
    pub fn with_mcp_servers(mut self, servers: Vec<McpServer>) -> Self {
        if self.tool_context.sandbox_policy() == SandboxPolicy::ReadOnly {
            return self;
        }
        // Start from zero, not the server's current count: a notification
        // that arrives between the startup listing and this builder call must
        // still trigger one re-list before the first request.
        self.mcp_tool_generations = servers
            .iter()
            .map(|server| (server.name.clone(), 0))
            .collect();
        self.mcp_servers.clone_from(&servers);
        let supports_images = self.config.model_config.supports_images;
        let enabled_tool_names = self.enabled_tool_names.clone();
        for server in servers {
            let server_name = server.name.clone();
            let client = Arc::clone(&server.client);
            for tool in &server.tools {
                let enabled = enabled_tool_names
                    .as_ref()
                    .is_none_or(|names| names.iter().any(|name| name == &tool.registered_name));
                if !enabled {
                    continue;
                }
                match server.exposure {
                    crate::config::mcp::McpExposure::Direct => {
                        self.register_mcp_tool(
                            &server_name,
                            tool,
                            Arc::clone(&client),
                            supports_images,
                        );
                    },
                    crate::config::mcp::McpExposure::Deferred => {
                        self.deferred_mcp_tools.push(DeferredMcpTool {
                            server: server_name.clone(),
                            tool: tool.clone(),
                            client: Arc::clone(&client),
                        });
                    },
                    crate::config::mcp::McpExposure::Hidden => {},
                }
            }
        }
        self.refresh_tool_search();
        self
    }

    /// Register one MCP tool unless its name is already taken.
    fn register_mcp_tool(
        &mut self,
        server: &str,
        tool: &McpTool,
        client: Arc<McpClient>,
        supports_images: bool,
    ) -> bool {
        if self.tools.has(&tool.registered_name) {
            tracing::warn!(
                target: "cake",
                "MCP server '{}' tool '{}' collides with an existing registered tool '{}'; skipping it",
                server,
                tool.name,
                tool.registered_name
            );
            return false;
        }
        self.tools
            .push_entry(mcp_tool_entry(server, tool, client, supports_images));
        true
    }

    /// Install `ToolSearch`, or fall back to direct registration when the
    /// exact-name allowlist or a name collision leaves no search surface.
    fn refresh_tool_search(&mut self) {
        if self.tool_search_installed {
            self.tools.remove_by_name(TOOL_SEARCH_NAME);
            self.tool_search_installed = false;
        }
        if self.deferred_mcp_tools.is_empty() {
            return;
        }
        if !tool_search_enabled(self.enabled_tool_names.as_deref()) {
            self.register_deferred_tools_direct();
            return;
        }
        if self.tools.has(TOOL_SEARCH_NAME) {
            tracing::warn!(
                target: "cake",
                "an existing tool already uses the registered name '{TOOL_SEARCH_NAME}'; \
                 registering deferred MCP tools directly"
            );
            self.register_deferred_tools_direct();
            return;
        }
        let entries: Vec<ToolSearchEntry> = self
            .deferred_mcp_tools
            .iter()
            .filter(|deferred| self.mcp_tool_enabled(&deferred.tool.registered_name))
            .map(|deferred| ToolSearchEntry {
                server: deferred.server.clone(),
                registered_name: deferred.tool.registered_name.clone(),
                description: deferred.tool.description.clone(),
                input_schema: deferred.tool.input_schema.clone(),
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        self.tools.push_entry(tool_search_entry(
            entries,
            Arc::clone(&self.pending_tool_activations),
            Arc::clone(&self.active_mcp_tools),
        ));
        self.tool_search_installed = true;
    }

    /// Whether the exact-name allowlist permits one MCP tool.
    fn mcp_tool_enabled(&self, name: &str) -> bool {
        self.enabled_tool_names
            .as_ref()
            .is_none_or(|names| names.iter().any(|enabled| enabled == name))
    }

    /// Add every permitted deferred tool directly, used when search is not
    /// available. The catalog is kept so a later refresh can still diff it;
    /// names already in the registry are skipped.
    fn register_deferred_tools_direct(&mut self) {
        let supports_images = self.config.model_config.supports_images;
        let deferred = self.deferred_mcp_tools.clone();
        for deferred in deferred {
            if !self.mcp_tool_enabled(&deferred.tool.registered_name) {
                continue;
            }
            if self.tools.has(&deferred.tool.registered_name) {
                continue;
            }
            self.register_mcp_tool(
                &deferred.server,
                &deferred.tool,
                Arc::clone(&deferred.client),
                supports_images,
            );
        }
    }

    /// Re-list servers whose tool-change generation advanced since the last
    /// provider request, then reconcile the registry and deferred catalog.
    ///
    /// Runs between turns so every provider request carries one coherent tool
    /// set. A failed re-list keeps the previous surface: the server's tools
    /// remain usable, and the next notification tries again.
    pub(super) async fn refresh_changed_mcp_tools(&mut self) {
        let changed_servers: Vec<String> = self
            .mcp_servers
            .iter()
            .filter(|server| {
                server.tools_generation()
                    != self
                        .mcp_tool_generations
                        .get(&server.name)
                        .copied()
                        .unwrap_or_default()
            })
            .map(|server| server.name.clone())
            .collect();

        for name in changed_servers {
            let Some(index) = self
                .mcp_servers
                .iter()
                .position(|server| server.name == name)
            else {
                continue;
            };
            let mut server = self.mcp_servers[index].clone();
            let observed = server.tools_generation();
            let previous = server.tools.clone();
            match server.reload_tools().await {
                Ok(changed) => {
                    if changed {
                        self.reconcile_mcp_server_tools(&server, &previous);
                    }
                    self.mcp_servers[index] = server;
                },
                Err(error) => {
                    tracing::warn!(
                        target: "cake",
                        "MCP server '{name}' could not refresh its tool list: {error}; keeping the previous tools"
                    );
                },
            }
            // Record the pre-request generation. A notification that arrives
            // during the re-list makes the next loop run again rather than
            // being swallowed by this pass.
            self.mcp_tool_generations.insert(name, observed);
        }
    }

    /// Apply one server's new tool snapshot to the registry and catalog.
    ///
    /// The diff is by registered name. Removed tools disappear from the
    /// registry and the catalog, new tools join the surface appropriate to
    /// the server's exposure, and a changed definition replaces the old entry
    /// so the next request carries the new schema and description.
    fn reconcile_mcp_server_tools(&mut self, server: &McpServer, previous: &[McpTool]) {
        let advertised: HashSet<&str> = server
            .tools
            .iter()
            .map(|tool| tool.registered_name.as_str())
            .collect();
        let previous_by_name: HashMap<&str, &McpTool> = previous
            .iter()
            .map(|tool| (tool.registered_name.as_str(), tool))
            .collect();

        let mut added = 0usize;
        let mut removed = 0usize;
        let mut updated = 0usize;

        for old in previous {
            if advertised.contains(old.registered_name.as_str()) {
                continue;
            }
            removed += 1;
            self.remove_mcp_tool(&server.name, old);
        }

        for tool in &server.tools {
            match previous_by_name.get(tool.registered_name.as_str()) {
                Some(old) if *old == tool => continue,
                Some(_) => updated += 1,
                None => added += 1,
            }
            self.reconcile_mcp_tool(server, tool);
        }

        self.refresh_tool_search();
        Self::log_mcp_tool_change(&server.name, added, removed, updated);
    }

    /// Remove one tool from every model-visible surface.
    fn remove_mcp_tool(&mut self, server: &str, tool: &McpTool) {
        self.tools.remove_by_name(&tool.registered_name);
        self.active_mcp_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&tool.registered_name);
        self.deferred_mcp_tools.retain(|deferred| {
            deferred.server != server || deferred.tool.registered_name != tool.registered_name
        });
    }

    /// Apply one added or changed tool according to the server's exposure.
    fn reconcile_mcp_tool(&mut self, server: &McpServer, tool: &McpTool) {
        let enabled = self.mcp_tool_enabled(&tool.registered_name);
        match server.exposure {
            crate::config::mcp::McpExposure::Direct => {
                self.reconcile_direct_mcp_tool(server, tool, enabled);
            },
            crate::config::mcp::McpExposure::Deferred => {
                self.reconcile_deferred_mcp_tool(server, tool, enabled);
            },
            crate::config::mcp::McpExposure::Hidden => {},
        }
    }

    /// Replace a direct tool's registry entry.
    fn reconcile_direct_mcp_tool(&mut self, server: &McpServer, tool: &McpTool, enabled: bool) {
        self.tools.remove_by_name(&tool.registered_name);
        if enabled {
            self.register_mcp_tool(
                &server.name,
                tool,
                Arc::clone(&server.client),
                self.config.model_config.supports_images,
            );
        }
    }

    /// Replace a deferred tool's catalog entry and, when it was active, its
    /// registry entry.
    fn reconcile_deferred_mcp_tool(&mut self, server: &McpServer, tool: &McpTool, enabled: bool) {
        let active = self
            .active_mcp_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&tool.registered_name);
        self.deferred_mcp_tools.retain(|deferred| {
            deferred.server != server.name || deferred.tool.registered_name != tool.registered_name
        });
        if !enabled {
            if active {
                self.remove_mcp_tool(&server.name, tool);
            }
            return;
        }
        self.deferred_mcp_tools.push(DeferredMcpTool {
            server: server.name.clone(),
            tool: tool.clone(),
            client: Arc::clone(&server.client),
        });
        if active {
            self.active_mcp_tools
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&tool.registered_name);
            self.tools.remove_by_name(&tool.registered_name);
            if self.register_mcp_tool(
                &server.name,
                tool,
                Arc::clone(&server.client),
                self.config.model_config.supports_images,
            ) {
                self.active_mcp_tools
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(tool.registered_name.clone());
            }
        } else if self.tools.has(&tool.registered_name) {
            // The no-ToolSearch fallback registers deferred tools directly.
            // Replace the entry so a changed definition reaches the model.
            self.tools.remove_by_name(&tool.registered_name);
            _ = self.register_mcp_tool(
                &server.name,
                tool,
                Arc::clone(&server.client),
                self.config.model_config.supports_images,
            );
        }
    }

    /// Report a reconciled tool change when anything differed.
    fn log_mcp_tool_change(server: &str, added: usize, removed: usize, updated: usize) {
        if added + removed + updated > 0 {
            tracing::info!(
                target: "cake",
                "MCP server '{server}' changed its tools: {added} added, {removed} removed, {updated} updated"
            );
        }
    }

    /// Re-register tools that were active before this invocation.
    ///
    /// Names are revalidated against the live catalog and the current
    /// allowlist; a server that disappeared, changed exposure, or had a tool
    /// removed is ignored rather than failing the resume.
    pub fn with_active_mcp_tools(mut self, names: impl IntoIterator<Item = String>) -> Self {
        if self.tool_context.sandbox_policy() == SandboxPolicy::ReadOnly {
            return self;
        }
        let supports_images = self.config.model_config.supports_images;
        for name in names {
            if !self.mcp_tool_enabled(&name) {
                tracing::debug!(
                    target: "cake",
                    "Ignoring restored MCP tool '{name}': excluded by the tool allowlist"
                );
                continue;
            }
            if self.tools.has(&name) {
                self.active_mcp_tools
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(name);
                continue;
            }
            let Some(deferred) = self
                .deferred_mcp_tools
                .iter()
                .find(|deferred| deferred.tool.registered_name == name)
                .cloned()
            else {
                tracing::debug!(
                    target: "cake",
                    "Ignoring restored MCP tool '{name}': not present in the live deferred catalog"
                );
                continue;
            };
            if self.register_mcp_tool(
                &deferred.server,
                &deferred.tool,
                Arc::clone(&deferred.client),
                supports_images,
            ) {
                self.active_mcp_tools
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(name);
            }
        }
        self
    }

    /// Register tools selected by a `ToolSearch` call before the next request.
    /// A successful batch is appended to the session so resume can restore it.
    fn activate_pending_mcp_tools(&mut self) -> anyhow::Result<()> {
        let selected = {
            let mut pending = self
                .pending_tool_activations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *pending)
        };
        if selected.is_empty() {
            return Ok(());
        }
        let supports_images = self.config.model_config.supports_images;
        let mut activated = Vec::new();
        for name in selected {
            if !self.mcp_tool_enabled(&name) {
                continue;
            }
            let already_active = self
                .active_mcp_tools
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&name);
            if already_active {
                continue;
            }
            let Some(deferred) = self
                .deferred_mcp_tools
                .iter()
                .find(|deferred| deferred.tool.registered_name == name)
                .cloned()
            else {
                continue;
            };
            if !self.register_mcp_tool(
                &deferred.server,
                &deferred.tool,
                Arc::clone(&deferred.client),
                supports_images,
            ) {
                continue;
            }
            self.active_mcp_tools
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(name.clone());
            activated.push(name);
        }
        if activated.is_empty() {
            return Ok(());
        }
        activated.sort_unstable();
        let record = SessionRecord::McpToolsActivated {
            session_id: self.session_id.to_string(),
            task_id: self.task_id.to_string(),
            timestamp: chrono::Utc::now(),
            tools: activated,
        };
        self.persist_record(&record)
    }

    /// Restrict the registry to an exact-name allowlist. An absent selection
    /// leaves the registry unchanged; an empty selection removes every tool.
    pub fn with_enabled_tools(mut self, enabled_tools: Option<&[String]>) -> Self {
        self.enabled_tool_names = enabled_tools.map(<[String]>::to_vec);
        if let Some(enabled_tools) = enabled_tools {
            self.tools.retain_enabled_tools(enabled_tools);
        }
        self.refresh_tool_search();
        self.install_skill_search();
        self
    }

    /// Sets the tool registry (for test fixtures).
    #[cfg(test)]
    pub(super) fn with_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    /// Sets the accumulated permission denials (for test fixtures).
    #[cfg(test)]
    pub(super) fn with_permission_denials(mut self, denials: Vec<String>) -> Self {
        self.permission_denials = denials;
        self
    }

    /// Sets the max output tokens config value (for test fixtures).
    #[cfg(test)]
    pub const fn with_max_output_tokens(mut self, val: Option<u32>) -> Self {
        self.config.model_config.max_output_tokens = val;
        self
    }

    /// Sets the context window config value (for test fixtures).
    #[cfg(test)]
    pub const fn with_context_window(mut self, val: Option<u32>) -> Self {
        self.config.model_config.context_window = val;
        self
    }

    /// Sets the reasoning max tokens config value (for test fixtures).
    #[cfg(test)]
    pub const fn with_reasoning_max_tokens(mut self, val: Option<u32>) -> Self {
        self.config.model_config.reasoning_max_tokens = val;
        self
    }

    /// Returns the resolved provider model identifier.
    pub fn model_name(&self) -> &str {
        &self.config.model_config.model
    }

    pub fn session_telemetry_settings(
        &self,
        output_format: OutputFormat,
        model_config: Option<String>,
    ) -> SessionTelemetrySettings {
        SessionTelemetrySettings {
            api_type: self.config.model_config.api_type,
            output_format,
            max_output_tokens: self.config.model_config.max_output_tokens,
            reasoning_effort: self.config.model_config.reasoning_effort,
            reasoning_max_tokens: self.config.model_config.reasoning_max_tokens,
            model_config,
            base_url: Some(self.config.model_config.base_url.clone()),
        }
    }

    /// Returns the session ID.
    pub const fn session_id(&self) -> uuid::Uuid {
        self.session_id
    }

    /// Returns the task ID.
    #[cfg(test)]
    pub const fn task_id(&self) -> uuid::Uuid {
        self.task_id
    }

    /// Returns the accumulated permission denials.
    #[cfg(test)]
    pub fn permission_denials(&self) -> &[String] {
        &self.permission_denials
    }

    /// Returns accumulated usage across all API calls.
    pub const fn total_usage(&self) -> &Usage {
        &self.total_usage
    }

    /// Returns the usage reported by the most recent provider attempt, if any.
    ///
    /// The provider's `input_tokens` for that turn is the full request size
    /// (system prompt, tool schemas, and history), which is the current
    /// context occupancy after the turn.
    pub const fn last_usage(&self) -> Option<Usage> {
        self.last_usage
    }

    /// Returns the configured model context window in tokens, if set.
    pub const fn context_window(&self) -> Option<u32> {
        self.config.model_config.context_window
    }

    /// Returns the token budget remaining in the configured context window.
    ///
    /// Computed from the input tokens of the most recent provider attempt:
    /// the provider's count of the full request just sent. The next request grows
    /// the context by its own output and any client-added tool outputs, which
    /// cake does not tokenize, so callers should reserve a buffer (for example
    /// compact when remaining falls below a threshold) rather than estimate
    /// that delta. `None` when no window is configured or before the first
    /// turn; never negative when configured (saturates at zero once usage
    /// meets or exceeds the window).
    pub fn context_remaining_tokens(&self) -> Option<u64> {
        match (self.context_window(), self.last_usage) {
            (Some(window), Some(usage)) => {
                Some(u64::from(window).saturating_sub(usage.input_tokens))
            },
            _ => None,
        }
    }

    /// Returns the number of API calls made.
    pub const fn turn_count(&self) -> u32 {
        self.turn_count
    }

    /// Sets the session ID for a restored session.
    ///
    /// Use this when continuing a previous session to preserve the session ID.
    pub fn with_session_id(mut self, id: uuid::Uuid) -> Self {
        self.session_id = id;
        if let Some(telemetry) = &mut self.telemetry {
            telemetry.context.session_id = id.to_string();
        }
        self
    }

    /// Sets the task ID for the current invocation.
    pub const fn with_task_id(mut self, id: uuid::Uuid) -> Self {
        self.task_id = id;
        self
    }

    /// Sets the resolved `[[models]]` entry name for this run, reported on the
    /// `task_start` record so a live stream consumer can name the model
    /// serving the run.
    pub fn with_model_config_name(mut self, name: String) -> Self {
        self.model_config_name = Some(name);
        self
    }

    /// Sets a short no-data stall window for provider tests.
    #[cfg(test)]
    pub(super) const fn with_stall_window(mut self, window: std::time::Duration) -> Self {
        self.runner.with_stall_window(window);
        self
    }

    /// Sets accumulated usage (for test fixtures).
    #[cfg(test)]
    pub const fn with_total_usage(mut self, usage: Usage) -> Self {
        self.total_usage = usage;
        self
    }

    /// Sets the usage of the most recent provider attempt (for restored sessions
    /// and test fixtures).
    pub const fn with_last_usage(mut self, usage: Option<Usage>) -> Self {
        self.last_usage = usage;
        self
    }

    /// Sets the turn count (for test fixtures).
    #[cfg(test)]
    pub const fn with_turn_count(mut self, count: u32) -> Self {
        self.turn_count = count;
        self
    }

    /// Applies user-configured agent-loop resource limits that were already
    /// resolved while loading settings.
    pub const fn with_limits(mut self, limits: &ResolvedLimits) -> Self {
        self.max_turns = limits.max_turns;
        self.max_tool_calls = limits.max_tool_calls;
        self
    }

    /// Sets the conversation history from raw conversation items.
    ///
    /// Restore and fork seed through [`Self::with_context_seed`], which carries
    /// the latest checkpoint too. This remains the checkpoint-free builder used
    /// by tests.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "restore seeds through with_context_seed; this remains the checkpoint-free history builder for tests"
        )
    )]
    pub fn with_history(mut self, messages: Vec<ConversationItem>) -> anyhow::Result<Self> {
        self.conversation.with_restored_history(messages)?;
        Ok(self)
    }

    /// Restores raw history and the latest checkpoint from a session seed.
    ///
    /// Restore and fork both use this instead of [`Self::with_history`] so a
    /// checkpointed session projects the same request context a live run would.
    pub fn with_context_seed(mut self, seed: crate::types::ContextSeed) -> anyhow::Result<Self> {
        self.conversation.with_restored_context(seed)?;
        Ok(self)
    }

    /// Number of inline images in the request-visible conversation history.
    ///
    /// A run whose model lacks image support warns from this count before the
    /// first request. Request builders omit those images without rewriting the
    /// stored session.
    pub(crate) fn history_image_count(&self) -> usize {
        self.conversation
            .request_context()
            .iter()
            .map(ConversationItem::image_count)
            .sum()
    }

    /// The provider request context for the next turn (for tests).
    #[cfg(test)]
    pub(crate) fn request_context(&self) -> Vec<ConversationItem> {
        self.conversation.request_context().into_owned()
    }

    /// The active checkpoint, for tests that pin the restored envelope.
    #[cfg(test)]
    pub(crate) const fn active_checkpoint(&self) -> Option<&crate::types::CompactionData> {
        self.conversation.checkpoint()
    }

    /// Set the skill locations for path-watching `SkillActivated` records.
    ///
    /// These paths are checked when the Read tool is used. If the model reads
    /// a SKILL.md that matches a known skill path, a `SkillActivated` record is
    /// emitted once per skill per session.
    pub fn with_skill_locations(mut self, locations: HashMap<PathBuf, Skill>) -> Self {
        self.skill_locations = Arc::new(locations);
        self.install_skill_search();
        self
    }

    /// Install `SkillSearch` when the catalog is visible and the run can read
    /// a `SKILL.md`. The tool searches metadata only; it never opens a body.
    fn install_skill_search(&mut self) {
        if self.skill_search_installed {
            self.tools.remove_by_name(SKILL_SEARCH_NAME);
            self.skill_search_installed = false;
        }
        if self.skill_locations.is_empty()
            || !skill_search_enabled(self.enabled_tool_names.as_deref())
            || (!self.tools.has("Read") && !self.tools.has("Bash"))
        {
            return;
        }
        if self.tools.has(SKILL_SEARCH_NAME) {
            tracing::warn!(
                target: "cake",
                "an existing tool already uses the registered name '{SKILL_SEARCH_NAME}'; \
                 skill catalog omissions are disabled"
            );
            return;
        }
        let entries: Vec<SkillSearchEntry> = self
            .skill_locations
            .values()
            .map(|skill| SkillSearchEntry {
                name: skill.name.clone(),
                description: skill.description.clone(),
                location: skill.location.clone(),
            })
            .collect();
        self.tools.push_entry(skill_search_entry(entries));
        self.skill_search_installed = true;
    }

    /// Seeds the set of skill names already activated in this session.
    ///
    /// Restore and fork construction pass the names recorded in the source
    /// session, so a continued run does not re-emit a "first observed"
    /// `SkillActivated` record for skills activated before the interruption.
    ///
    /// Replaces the backing set instead of mutating it, so call this only
    /// during construction, before the agent runs. Anything holding a clone
    /// of the original set would not see a later replacement.
    pub fn with_activated_skills(mut self, names: HashSet<String>) -> Self {
        self.activated_skills = Arc::new(Mutex::new(names));
        self
    }

    /// Returns a copy of the activated-skill names (for test fixtures).
    #[cfg(test)]
    pub(crate) fn activated_skill_names(&self) -> HashSet<String> {
        self.activated_skills
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Enables command hooks for lifecycle and tool-call events.
    pub fn with_hook_runner(mut self, runner: Arc<HookRunner>) -> Self {
        self.hook_runner = Some(runner);
        self
    }

    /// Constrains the final response to validate against a compiled JSON
    /// Schema. Intermediate tool-use turns are unaffected.
    pub fn set_output_schema(&mut self, schema: Arc<OutputSchema>) {
        self.output_schema = Some(schema);
    }

    /// Builder form of [`Self::set_output_schema`] (for test fixtures).
    #[cfg(test)]
    pub(super) fn with_output_schema(mut self, schema: Arc<OutputSchema>) -> Self {
        self.set_output_schema(schema);
        self
    }

    /// Append hook-provided developer context before the next provider request.
    pub fn append_developer_context(&mut self, contexts: Vec<String>) {
        self.conversation.append_developer_context(contexts);
    }

    /// Append developer context stating the output-schema requirement for the
    /// final response. No-op when no schema is attached.
    pub fn append_output_schema_context(&mut self) {
        let Some(schema) = &self.output_schema else {
            return;
        };
        let schema_json =
            serde_json::to_string_pretty(&schema.raw).unwrap_or_else(|_| schema.raw.to_string());
        let message = format!(
            "The final response for this task (the last assistant message, \
             after all tool use is complete) must be a single JSON document \
             that validates against the following JSON Schema (draft 2020-12). \
             Respond with only the JSON document: no Markdown code fences and \
             no surrounding prose. Intermediate tool use and reasoning are \
             unaffected by this requirement.\n\n{schema_json}"
        );
        self.conversation.append_developer_context(vec![message]);
    }

    #[cfg(test)]
    /// Enables streaming JSON output for each message.
    ///
    /// The callback receives a JSON string for each message, tool call, and result.
    /// This is useful for integrating with other tools or TUIs.
    pub fn with_streaming_json(mut self, callback: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.observer.set_streaming_json(callback);
        self
    }

    /// Enables streaming JSON output with a callback that can stop the run.
    ///
    /// Unlike the test-only `with_streaming_json` builder, errors from the
    /// callback propagate through the agent loop so the CLI can leave scope
    /// normally.
    pub(crate) fn with_fallible_streaming_json(
        mut self,
        callback: impl Fn(&str) -> anyhow::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.observer.set_fallible_streaming_json(callback);
        self
    }

    /// Enables live append-only persistence for each emitted task record.
    pub fn with_persist_callback(
        mut self,
        callback: impl FnMut(&SessionRecord) -> anyhow::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.observer.set_persist_callback(callback);
        self
    }

    /// Enables human-readable progress updates for recoverable agent-loop events.
    pub fn with_progress_callback(
        mut self,
        callback: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        self.observer.set_progress(callback);
        self
    }

    /// Enables best-effort session telemetry sidecar writing.
    ///
    /// The writer is shared with a gate telemetry sink installed into the
    /// tool context, so gate decisions are recorded as soon as a check
    /// completes instead of waiting for the tool result.
    pub fn with_session_telemetry(
        mut self,
        writer: SessionTelemetryWriter,
        invocation_id: uuid::Uuid,
    ) -> Self {
        let context = SessionTelemetryContext {
            session_id: self.session_id.to_string(),
            invocation_id: invocation_id.to_string(),
        };
        let writer = Arc::new(SharedSessionTelemetryWriter::new(writer));
        self.telemetry = Some(AgentTelemetry {
            context: context.clone(),
            writer: Arc::clone(&writer),
        });
        if let Some(pipeline) = self.tool_context.gates.clone() {
            let gate_sink = GateTelemetrySink::new(Arc::clone(&writer), context);
            pipeline.install_telemetry_sink(gate_sink.clone());
            for startup in pipeline.startup_records() {
                gate_sink.record_startup(startup);
            }
        }
        self
    }

    /// Emit a task record to persistence and streaming sinks.
    fn stream_record(&mut self, record: StreamRecord) -> anyhow::Result<()> {
        self.observer.stream_record(record)
    }

    /// Persist a session-only audit record without emitting it to stream-json.
    fn persist_record(&mut self, record: &SessionRecord) -> anyhow::Result<()> {
        self.observer.persist_record(record)
    }

    /// Stream a conversation item as JSON via the streaming callback, if set.
    fn stream_item(&mut self, item: &ConversationItem) -> anyhow::Result<()> {
        self.observer.stream_item(item)
    }

    fn stream_item_with_replay(
        &mut self,
        item: &ConversationItem,
        replay: Option<ReplaySafety>,
    ) -> anyhow::Result<()> {
        self.observer.stream_item_with_replay(item, replay)
    }

    fn report_progress(&self, message: &str) {
        self.observer.report_progress(message);
    }

    /// Emit the task start record.
    pub fn emit_task_start_record(&mut self) -> anyhow::Result<()> {
        let record = StreamRecord::TaskStart(TaskStartData {
            session_id: self.session_id.to_string(),
            task_id: self.task_id.to_string(),
            timestamp: chrono::Utc::now(),
            model: Some(self.config.model_config.model.clone()),
            model_config: self.model_config_name.clone(),
        });

        self.stream_record(record)
    }

    /// Emit append-only audit records for mutable prompt context used by this task.
    pub fn emit_prompt_context_records(&mut self) -> anyhow::Result<()> {
        let timestamp = chrono::Utc::now();
        let Self {
            conversation,
            session_id,
            task_id,
            observer,
            ..
        } = &mut *self;

        let session_id = session_id.to_string();
        let task_id = task_id.to_string();

        for item in conversation.history().iter().take_while(|item| {
            matches!(
                item,
                ConversationItem::Message {
                    role: Role::System | Role::Developer,
                    ..
                }
            )
        }) {
            if let ConversationItem::Message {
                role: Role::Developer,
                content,
                ..
            } = item
            {
                observer.persist_record(&SessionRecord::PromptContext {
                    session_id: session_id.clone(),
                    task_id: task_id.clone(),
                    role: Role::Developer,
                    content: content.clone(),
                    timestamp,
                })?;
            }
        }
        Ok(())
    }

    /// Emit the repair outputs synthesized while restoring history.
    ///
    /// A restored conversation can contain a function call whose output the
    /// previous process never persisted. [`Self::with_history`] closes each
    /// such call in memory; this appends the same items to the session file so
    /// later restores read a well-formed history. No-op for a fresh session or
    /// a restored one whose calls are all matched.
    pub fn emit_history_repair_records(&mut self) -> anyhow::Result<()> {
        for item in self.conversation.take_pending_repairs() {
            self.stream_item(&item)?;
        }
        Ok(())
    }

    #[cfg(test)]
    /// Accumulate usage from an API turn.
    const fn accumulate_usage(&mut self, turn_usage: Option<&Usage>) {
        accumulate_usage(&mut self.total_usage, turn_usage);
    }

    /// Emit the task completion record with success/error and usage stats.
    pub fn emit_task_complete_record(
        &mut self,
        outcome: TaskOutcome,
        duration_ms: u64,
    ) -> anyhow::Result<()> {
        let permission_denials = if self.permission_denials.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.permission_denials))
        };
        let record = StreamRecord::TaskComplete(TaskCompleteData {
            outcome,
            duration_ms,
            turn_count: self.turn_count,
            tool_call_count: self.tool_call_count,
            session_id: self.session_id.to_string(),
            task_id: self.task_id.to_string(),
            usage: self.total_usage,
            permission_denials,
        });

        self.stream_record(record)
    }

    /// Emit the final telemetry summary for this CLI invocation.
    pub fn emit_session_summary_telemetry(
        &mut self,
        success: bool,
        duration_ms: u64,
        error: Option<String>,
    ) {
        let Some(context) = self.telemetry_context() else {
            return;
        };
        let record = SessionTelemetryRecord::SessionSummary {
            session_id: context.session_id,
            invocation_id: context.invocation_id,
            timestamp: chrono::Utc::now(),
            success,
            duration_ms,
            turn_count: self.turn_count,
            usage: self.total_usage,
            error,
        };
        self.append_telemetry_record(&record);
    }
}

#[cfg(test)]
#[path = "agent/agent_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "agent/response_stall_tests.rs"]
mod response_stall_tests;

#[cfg(test)]
#[path = "agent/gate_telemetry_tests.rs"]
mod gate_telemetry_tests;

#[cfg(test)]
mod session_restore_tests;

#[cfg(test)]
mod tail_only_growth_tests;
