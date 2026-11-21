//! Tool-call scheduling for a single assistant turn.
//!
//! Tool calls in one assistant turn execute concurrently. To keep that safe
//! when the model issues a Read and a mutation for the same file, the
//! scheduler groups path-targeted calls by canonical target path whenever the
//! turn mutates that path: groups run concurrently with each other and with
//! untargeted calls, while calls within a group run sequentially in issue
//! order, each seeing the previous call's effects. A read of a path the turn
//! does not mutate stays in its own group, so independent reads keep running
//! concurrently.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::clients::tools::{PathTouch, ToolContext, ToolRegistry};
use crate::hooks::ToolHookPlan;

/// A tool call scheduled for execution, tagged with its position in the
/// model's issue order so results can be re-emitted in that order.
#[derive(Debug)]
pub struct ScheduledToolCall {
    pub index: usize,
    pub call_id: String,
    pub name: String,
    pub plan: ToolHookPlan,
    /// Canonical path this call targets for scheduling, when it was resolved
    /// for grouping. Every member of a serialized same-path group carries the
    /// group's path, including the group leader; a call whose target was not
    /// needed for grouping (for example, a read in a turn with no resolved
    /// mutation) keeps `None`.
    pub target: Option<PathBuf>,
}

/// Partition a turn's tool-call plans into execution groups.
///
/// Executable calls that resolve a canonical path share a group when the turn
/// also mutates that path, in issue order: a read observes the state left by
/// the calls issued before it. Every other call --- tools that declare no
/// path target, hook-blocked calls, calls whose arguments or path cannot be
/// resolved, and reads of paths the turn does not mutate --- gets a singleton
/// group. Groups are safe to run concurrently with each other as long as
/// members of a group run sequentially in the returned order.
///
/// Targets are resolved in two phases: mutating calls first, then the
/// remaining calls only when some mutation resolved. A turn without a
/// resolved mutation cannot serialize anything, so the observe resolvers
/// --- a full path validation on the model's read path --- are skipped and
/// every call keeps `target: None`.
pub fn schedule_tool_calls(
    tools: &ToolRegistry,
    context: &ToolContext,
    tool_plans: Vec<(String, String, ToolHookPlan)>,
) -> Vec<Vec<ScheduledToolCall>> {
    let expected_groups = tool_plans.len();
    let resolved = resolve_scheduling_targets(tools, context, &tool_plans);
    let mutated: HashSet<PathBuf> = resolved
        .iter()
        .flatten()
        .filter(|(_, touch)| *touch == PathTouch::Mutate)
        .map(|(path, _)| path.clone())
        .collect();

    let mut groups: Vec<Vec<ScheduledToolCall>> = Vec::with_capacity(expected_groups);
    let mut path_groups: HashMap<PathBuf, usize> = HashMap::new();

    for ((index, (call_id, name, plan)), resolved_target) in
        tool_plans.into_iter().enumerate().zip(resolved)
    {
        let (target, serialized) = match resolved_target {
            Some((path, touch)) => {
                let serialized = touch == PathTouch::Mutate || mutated.contains(&path);
                (Some(path), serialized)
            },
            None => (None, false),
        };
        let call = ScheduledToolCall {
            index,
            call_id,
            name,
            plan,
            target,
        };

        let group = call
            .target
            .as_ref()
            .filter(|_| serialized)
            .and_then(|path| path_groups.get(path).copied());
        if let Some(group) = group {
            groups[group].push(call);
        } else {
            if serialized && let Some(path) = call.target.as_ref() {
                path_groups.insert(path.clone(), groups.len());
            }
            groups.push(vec![call]);
        }
    }

    groups
}

/// Resolve the path targets same-path scheduling needs: mutating calls first,
/// then the remaining calls only when some mutation resolved. A mutation may
/// precede a same-path read in issue order, so it is resolved before grouping
/// regardless of its position; and a turn without a resolved mutation cannot
/// serialize anything, so the observe resolvers --- a full path validation on
/// the model's read path --- are skipped and calls that can never group keep
/// no target.
fn resolve_scheduling_targets(
    tools: &ToolRegistry,
    context: &ToolContext,
    tool_plans: &[(String, String, ToolHookPlan)],
) -> Vec<Option<(PathBuf, PathTouch)>> {
    let mut resolved: Vec<Option<(PathBuf, PathTouch)>> = tool_plans
        .iter()
        .map(|(_, name, plan)| match plan {
            ToolHookPlan::Execute { arguments, .. }
                if tools.path_touch(name) == Some(PathTouch::Mutate) =>
            {
                scheduling_target(tools, context, name, arguments)
            },
            ToolHookPlan::Execute { .. } | ToolHookPlan::Block { .. } => None,
        })
        .collect();
    let mutation_resolved = resolved
        .iter()
        .flatten()
        .any(|(_, touch)| *touch == PathTouch::Mutate);

    if mutation_resolved {
        for (slot, (_, name, plan)) in resolved.iter_mut().zip(tool_plans) {
            if slot.is_none()
                && let ToolHookPlan::Execute { arguments, .. } = plan
            {
                *slot = scheduling_target(tools, context, name, arguments);
            }
        }
    }

    resolved
}

/// Resolve one call's scheduling target. Calls with unparseable arguments or
/// invalid paths have no target: they execute as scheduled and surface their
/// own errors.
fn scheduling_target(
    tools: &ToolRegistry,
    context: &ToolContext,
    name: &str,
    arguments: &str,
) -> Option<(PathBuf, PathTouch)> {
    match tools.path_target(context, name, arguments) {
        Some((Ok(path), touch)) => Some((path, touch)),
        Some((Err(_), _)) | None => None,
    }
}
