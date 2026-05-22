use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::clients::tools::{ToolContext, ToolRegistry};
use crate::config::skills::Skill;

#[derive(Debug, Clone)]
pub(super) struct SkillActivation {
    pub(super) name: String,
    pub(super) path: PathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct ToolExecutionOutput {
    pub(super) output: String,
    pub(super) skill_activation: Option<SkillActivation>,
}

#[derive(Debug, Default)]
pub(super) struct SkillActivations {
    pub(super) active: HashSet<String>,
    pub(super) pending: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SkillReservation {
    Reserved,
    AlreadyActive,
    AlreadyPending,
}

impl SkillActivations {
    pub(super) fn replace_active(&mut self, skills: HashSet<String>) {
        self.active = skills;
        self.pending.clear();
    }

    pub(super) fn reserve(&mut self, name: &str) -> SkillReservation {
        if self.active.contains(name) {
            return SkillReservation::AlreadyActive;
        }
        if !self.pending.insert(name.to_string()) {
            return SkillReservation::AlreadyPending;
        }
        SkillReservation::Reserved
    }

    pub(super) fn complete(&mut self, name: &str) {
        self.pending.remove(name);
        self.active.insert(name.to_string());
    }

    pub(super) fn fail(&mut self, name: &str) {
        self.pending.remove(name);
    }
}

pub(super) async fn execute_tool_output(
    tools: &ToolRegistry,
    context: Arc<ToolContext>,
    name: &str,
    arguments: &str,
) -> Result<String, String> {
    tools
        .execute(context, name, arguments)
        .await
        .map(|result| result.output)
}

pub(super) async fn execute_tool_with_skill_dedup(
    tools: &ToolRegistry,
    context: Arc<ToolContext>,
    name: &str,
    arguments: &str,
    skill_locations: &HashMap<PathBuf, Skill>,
    skill_activations: &Arc<Mutex<SkillActivations>>,
) -> Result<ToolExecutionOutput, String> {
    if name != "Read" {
        return execute_tool_output(tools, context, name, arguments)
            .await
            .map(|output| ToolExecutionOutput {
                output,
                skill_activation: None,
            });
    }

    let Some(path_str) = crate::clients::tools::read_extract_path(arguments) else {
        return execute_tool_output(tools, context, name, arguments)
            .await
            .map(|output| ToolExecutionOutput {
                output,
                skill_activation: None,
            });
    };

    let Ok(path) = PathBuf::from(&path_str).canonicalize() else {
        return execute_tool_output(tools, context, name, arguments)
            .await
            .map(|output| ToolExecutionOutput {
                output,
                skill_activation: None,
            });
    };

    let Some(skill) = skill_locations.get(&path) else {
        return execute_tool_output(tools, context, name, arguments)
            .await
            .map(|output| ToolExecutionOutput {
                output,
                skill_activation: None,
            });
    };
    let skill_name = &skill.name;

    let reservation = skill_activations
        .lock()
        .unwrap_or_else(|e| {
            tracing::error!("skill_activations mutex poisoned, recovering: {e}");
            e.into_inner()
        })
        .reserve(skill_name);
    match reservation {
        SkillReservation::Reserved => {},
        SkillReservation::AlreadyActive => {
            tracing::info!("Skill '{skill_name}' already activated, skipping re-read");
            return Ok(ToolExecutionOutput {
                output: format!(
                    "Skill '{skill_name}' is already active in this session. \
                     Its instructions are already in the conversation context."
                ),
                skill_activation: None,
            });
        },
        SkillReservation::AlreadyPending => {
            tracing::info!("Skill '{skill_name}' activation already in progress, skipping re-read");
            return Ok(ToolExecutionOutput {
                output: format!(
                    "Skill '{skill_name}' activation is already in progress in this tool batch. \
                     Its instructions will be included when that read completes."
                ),
                skill_activation: None,
            });
        },
    }

    let output = match skill.load_body() {
        Ok(output) => output,
        Err(error) => {
            skill_activations
                .lock()
                .unwrap_or_else(|e| {
                    tracing::error!("skill_activations mutex poisoned, recovering: {e}");
                    e.into_inner()
                })
                .fail(skill_name);
            return Err(format!(
                "Failed to load skill '{}': {error}",
                skill.location.display()
            ));
        },
    };
    skill_activations
        .lock()
        .unwrap_or_else(|e| {
            tracing::error!("skill_activations mutex poisoned, recovering: {e}");
            e.into_inner()
        })
        .complete(skill_name);
    tracing::info!("Skill '{}' activated", skill_name);
    Ok(ToolExecutionOutput {
        output,
        skill_activation: Some(SkillActivation {
            name: skill_name.clone(),
            path,
        }),
    })
}
