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
//! See ADR-013 and ADR-043.

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
    // Phase one resolves mutating targets only. A mutation may precede a
    // same-path read in issue order, so it is resolved before grouping
    // regardless of its position.
    let mut resolved: Vec<Option<(PathBuf, PathTouch)>> = tool_plans
        .iter()
        .map(|(_, name, plan)| match plan {
            ToolHookPlan::Execute { arguments, .. }
                if tools.path_touch(name) == Some(PathTouch::Mutate) =>
            {
                match tools.path_target(context, name, arguments) {
                    // Calls with unparseable arguments or invalid paths execute as
                    // scheduled and surface their own errors.
                    Some((Ok(path), touch)) => Some((path, touch)),
                    Some((Err(_), _)) | None => None,
                }
            },
            ToolHookPlan::Execute { .. } | ToolHookPlan::Block { .. } => None,
        })
        .collect();
    let mutated: HashSet<PathBuf> = resolved
        .iter()
        .flatten()
        .filter(|(_, touch)| *touch == PathTouch::Mutate)
        .map(|(path, _)| path.clone())
        .collect();

    if !mutated.is_empty() {
        // Phase two: a mutation resolved, so a same-path read must join its
        // group. Resolve the remaining calls against the same grant table.
        for (slot, (_, name, plan)) in resolved.iter_mut().zip(&tool_plans) {
            if slot.is_none()
                && let ToolHookPlan::Execute { arguments, .. } = plan
                && let Some((Ok(path), touch)) = tools.path_target(context, name, arguments)
            {
                *slot = Some((path, touch));
            }
        }
    }

    let mut groups: Vec<Vec<ScheduledToolCall>> = Vec::with_capacity(expected_groups);
    let mut path_groups: HashMap<PathBuf, usize> = HashMap::new();

    for ((index, (call_id, name, plan)), resolved_target) in
        tool_plans.into_iter().enumerate().zip(resolved)
    {
        let (target, serialized) = match resolved_target {
            Some((path, touch)) => {
                // Mutations always serialize with themselves; a read joins the
                // group only when the turn also mutates its path.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::tools::{ToolContext, default_tool_registry};
    use crate::hooks::ToolHookPlan;
    use tempfile::TempDir;

    fn execute_plan(arguments: String) -> ToolHookPlan {
        ToolHookPlan::Execute {
            arguments,
            prefix_notice: None,
            additional_context: Vec::new(),
        }
    }

    fn blocked_plan(reason: String) -> ToolHookPlan {
        ToolHookPlan::Block {
            reason,
            additional_context: Vec::new(),
        }
    }

    fn scheduling_fixture() -> (TempDir, ToolContext, PathBuf, PathBuf) {
        let dir = TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
        let first_path = dir.path().join("first.txt");
        let second_path = dir.path().join("second.txt");
        std::fs::write(&first_path, "first").unwrap();
        std::fs::write(&second_path, "second").unwrap();
        let context = ToolContext::with_temp_dirs(
            dir.path().canonicalize().unwrap(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        (dir, context, first_path, second_path)
    }

    fn schedule(
        context: &ToolContext,
        tool_plans: Vec<(String, String, ToolHookPlan)>,
    ) -> Vec<Vec<ScheduledToolCall>> {
        schedule_tool_calls(&default_tool_registry(), context, tool_plans)
    }

    /// Reduce groups to the original call indexes for compact assertions.
    fn group_indexes(groups: &[Vec<ScheduledToolCall>]) -> Vec<Vec<usize>> {
        groups
            .iter()
            .map(|group| group.iter().map(|call| call.index).collect())
            .collect()
    }

    fn edit_arguments(path: &std::path::Path) -> String {
        serde_json::json!({
            "path": path,
            "edits": [{ "old_text": "first", "new_text": "updated" }]
        })
        .to_string()
    }

    #[test]
    fn serializes_second_edit_for_same_file() {
        let (_dir, context, path, _) = scheduling_fixture();
        let arguments = edit_arguments(&path);

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan(arguments.clone()),
                ),
                (
                    "call-2".to_string(),
                    "Edit".to_string(),
                    execute_plan(arguments),
                ),
            ],
        );

        assert_eq!(group_indexes(&groups), vec![vec![0, 1]]);
        assert_eq!(groups[0][0].call_id, "call-1");
        assert_eq!(groups[0][1].call_id, "call-2");
    }

    #[test]
    fn serializes_write_after_edit_for_same_file() {
        let (_dir, context, path, _) = scheduling_fixture();
        let write_arguments = serde_json::json!({
            "path": path,
            "content": "replacement"
        })
        .to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&path)),
                ),
                (
                    "call-2".to_string(),
                    "Write".to_string(),
                    execute_plan(write_arguments),
                ),
            ],
        );

        assert_eq!(group_indexes(&groups), vec![vec![0, 1]]);
    }

    #[test]
    fn serializes_relative_and_absolute_paths_for_same_file() {
        let (_dir, context, path, _) = scheduling_fixture();
        let current_dir = std::env::current_dir().unwrap();
        let relative_path = path.strip_prefix(current_dir).unwrap();
        let relative_arguments = serde_json::json!({
            "path": relative_path,
            "content": "replacement"
        })
        .to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&path)),
                ),
                (
                    "call-2".to_string(),
                    "Write".to_string(),
                    execute_plan(relative_arguments),
                ),
            ],
        );

        assert_eq!(group_indexes(&groups), vec![vec![0, 1]]);
    }

    #[test]
    fn mutations_to_different_files_stay_concurrent() {
        let (_dir, context, first_path, second_path) = scheduling_fixture();
        let second_arguments = serde_json::json!({
            "path": second_path,
            "content": "replacement"
        })
        .to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&first_path)),
                ),
                (
                    "call-2".to_string(),
                    "Write".to_string(),
                    execute_plan(second_arguments),
                ),
            ],
        );

        assert_eq!(group_indexes(&groups), vec![vec![0], vec![1]]);
    }

    #[test]
    fn repeated_reads_for_same_file_stay_concurrent() {
        let (_dir, context, path, _) = scheduling_fixture();
        let read_arguments = serde_json::json!({ "path": path }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Read".to_string(),
                    execute_plan(read_arguments.clone()),
                ),
                (
                    "call-2".to_string(),
                    "Read".to_string(),
                    execute_plan(read_arguments),
                ),
            ],
        );

        // The turn mutates nothing, so the reads stay independent singletons
        // and scheduling skips their path resolution entirely.
        assert_eq!(group_indexes(&groups), vec![vec![0], vec![1]]);
        assert!(groups.iter().flatten().all(|call| call.target.is_none()));
    }

    #[test]
    fn unresolvable_mutation_leaves_observe_targets_unresolved() {
        let (_dir, context, path, _) = scheduling_fixture();
        let read_arguments = serde_json::json!({ "path": path }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Read".to_string(),
                    execute_plan(read_arguments),
                ),
                (
                    "call-2".to_string(),
                    "Edit".to_string(),
                    execute_plan("{ not json".to_string()),
                ),
            ],
        );

        // No mutation resolved, so nothing can serialize and the read's
        // target stays unresolved too.
        assert_eq!(group_indexes(&groups), vec![vec![0], vec![1]]);
        assert!(groups.iter().flatten().all(|call| call.target.is_none()));
    }

    #[test]
    fn serializes_read_before_edit_for_same_file() {
        let (_dir, context, path, _) = scheduling_fixture();
        let read_arguments = serde_json::json!({ "path": path }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Read".to_string(),
                    execute_plan(read_arguments),
                ),
                (
                    "call-2".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&path)),
                ),
            ],
        );

        assert_eq!(group_indexes(&groups), vec![vec![0, 1]]);
        assert_eq!(groups[0][0].call_id, "call-1");
        assert_eq!(groups[0][1].call_id, "call-2");
    }

    #[test]
    fn serializes_edit_before_read_for_same_file() {
        let (_dir, context, path, _) = scheduling_fixture();
        let read_arguments = serde_json::json!({ "path": path }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&path)),
                ),
                (
                    "call-2".to_string(),
                    "Read".to_string(),
                    execute_plan(read_arguments),
                ),
            ],
        );

        // The read observes the edit's effects instead of racing them.
        assert_eq!(group_indexes(&groups), vec![vec![0, 1]]);
        assert_eq!(groups[0][0].call_id, "call-1");
        assert_eq!(groups[0][1].call_id, "call-2");
    }

    #[test]
    fn serializes_read_and_write_for_same_nonexistent_file() {
        let (dir, context, _, _) = scheduling_fixture();
        let path = dir.path().join("nonexistent.txt");
        assert!(!path.exists());
        let read_arguments = serde_json::json!({ "path": path }).to_string();
        let write_arguments =
            serde_json::json!({ "path": path, "content": "new file content" }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Read".to_string(),
                    execute_plan(read_arguments),
                ),
                (
                    "call-2".to_string(),
                    "Write".to_string(),
                    execute_plan(write_arguments),
                ),
            ],
        );

        // The read observes the pre-write state deterministically instead of
        // racing whether the file exists yet.
        assert_eq!(group_indexes(&groups), vec![vec![0, 1]]);
    }

    #[test]
    fn serializes_write_before_read_for_same_nonexistent_file() {
        let (dir, context, _, _) = scheduling_fixture();
        let path = dir.path().join("nonexistent.txt");
        assert!(!path.exists());
        let write_arguments =
            serde_json::json!({ "path": path, "content": "new file content" }).to_string();
        let read_arguments = serde_json::json!({ "path": path }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Write".to_string(),
                    execute_plan(write_arguments),
                ),
                (
                    "call-2".to_string(),
                    "Read".to_string(),
                    execute_plan(read_arguments),
                ),
            ],
        );

        // The read observes the write's result instead of racing the creation.
        assert_eq!(group_indexes(&groups), vec![vec![0, 1]]);
        assert_eq!(groups[0][0].call_id, "call-1");
        assert_eq!(groups[0][1].call_id, "call-2");
    }

    #[test]
    fn serializes_read_image_behind_same_path_edit() {
        let (_dir, context, path, _) = scheduling_fixture();
        let read_image_arguments = serde_json::json!({ "path": path }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&path)),
                ),
                (
                    "call-2".to_string(),
                    "ReadImage".to_string(),
                    execute_plan(read_image_arguments),
                ),
            ],
        );

        // ReadImage declares the same observing target as Read.
        assert_eq!(group_indexes(&groups), vec![vec![0, 1]]);
        assert_eq!(groups[0][0].call_id, "call-1");
        assert_eq!(groups[0][1].call_id, "call-2");
    }

    #[test]
    fn read_of_unmutated_path_stays_concurrent_with_other_mutations() {
        let (_dir, context, first_path, second_path) = scheduling_fixture();
        let read_arguments = serde_json::json!({ "path": second_path }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Read".to_string(),
                    execute_plan(read_arguments),
                ),
                (
                    "call-2".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&first_path)),
                ),
            ],
        );

        assert_eq!(group_indexes(&groups), vec![vec![0], vec![1]]);
        assert_eq!(
            groups[0][0].target,
            Some(second_path.canonicalize().unwrap())
        );
        assert_eq!(
            groups[1][0].target,
            Some(first_path.canonicalize().unwrap())
        );
    }

    #[test]
    fn interleaved_same_path_calls_keep_issue_order() {
        let (_dir, context, path, _) = scheduling_fixture();
        let read_arguments = serde_json::json!({ "path": path }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Read".to_string(),
                    execute_plan(read_arguments.clone()),
                ),
                (
                    "call-2".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&path)),
                ),
                (
                    "call-3".to_string(),
                    "Read".to_string(),
                    execute_plan(read_arguments),
                ),
            ],
        );

        assert_eq!(group_indexes(&groups), vec![vec![0, 1, 2]]);
        assert_eq!(groups[0][0].call_id, "call-1");
        assert_eq!(groups[0][1].call_id, "call-2");
        assert_eq!(groups[0][2].call_id, "call-3");
    }

    #[test]
    fn every_serialized_member_retains_the_group_target() {
        let (dir, context, first_path, _) = scheduling_fixture();
        let path = dir.path().join("nonexistent.txt");
        assert!(!path.exists());
        let read_arguments = serde_json::json!({ "path": path }).to_string();
        let write_arguments =
            serde_json::json!({ "path": path, "content": "new file content" }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Read".to_string(),
                    execute_plan(read_arguments),
                ),
                (
                    "call-2".to_string(),
                    "Write".to_string(),
                    execute_plan(write_arguments),
                ),
                (
                    "call-3".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&first_path)),
                ),
            ],
        );

        let expected = dir.path().canonicalize().unwrap().join("nonexistent.txt");
        assert_eq!(group_indexes(&groups), vec![vec![0, 1], vec![2]]);
        // The group leader keeps the target promised by the struct contract;
        // followers keep it too.
        assert_eq!(groups[0][0].target, Some(expected.clone()));
        assert_eq!(groups[0][1].target, Some(expected));
    }

    #[test]
    fn serializes_second_write_for_same_nonexistent_file() {
        let (dir, context, _, _) = scheduling_fixture();
        let path = dir.path().join("nonexistent.txt");
        assert!(!path.exists());
        let arguments =
            serde_json::json!({ "path": path, "content": "new file content" }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Write".to_string(),
                    execute_plan(arguments.clone()),
                ),
                (
                    "call-2".to_string(),
                    "Write".to_string(),
                    execute_plan(arguments),
                ),
            ],
        );

        assert_eq!(group_indexes(&groups), vec![vec![0, 1]]);
    }

    #[test]
    fn serializes_write_then_edit_for_same_nonexistent_file() {
        let (dir, context, _, _) = scheduling_fixture();
        let path = dir.path().join("nonexistent.txt");
        assert!(!path.exists());
        let write_args =
            serde_json::json!({ "path": path, "content": "new file content" }).to_string();
        let edit_args = edit_arguments(&path);

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Write".to_string(),
                    execute_plan(write_args),
                ),
                (
                    "call-2".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_args),
                ),
            ],
        );

        assert_eq!(group_indexes(&groups), vec![vec![0, 1]]);
        assert_eq!(groups[0][0].call_id, "call-1");
        assert_eq!(groups[0][1].call_id, "call-2");
    }

    #[test]
    fn serializes_edit_then_write_for_same_nonexistent_file() {
        let (dir, context, _, _) = scheduling_fixture();
        let path = dir.path().join("nonexistent.txt");
        assert!(!path.exists());
        let edit_args = edit_arguments(&path);
        let write_args =
            serde_json::json!({ "path": path, "content": "new file content" }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_args),
                ),
                (
                    "call-2".to_string(),
                    "Write".to_string(),
                    execute_plan(write_args),
                ),
            ],
        );

        assert_eq!(group_indexes(&groups), vec![vec![0, 1]]);
        assert_eq!(groups[0][0].call_id, "call-1");
        assert_eq!(groups[0][1].call_id, "call-2");
    }

    #[test]
    fn interleaved_calls_group_by_path_and_keep_issue_order() {
        let (_dir, context, first_path, second_path) = scheduling_fixture();
        let read_arguments = serde_json::json!({ "path": second_path }).to_string();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&first_path)),
                ),
                (
                    "call-2".to_string(),
                    "Read".to_string(),
                    execute_plan(read_arguments),
                ),
                (
                    "call-3".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&first_path)),
                ),
            ],
        );

        assert_eq!(group_indexes(&groups), vec![vec![0, 2], vec![1]]);
    }

    #[test]
    fn hook_blocked_call_is_not_serialized_with_same_path_mutation() {
        let (_dir, context, path, _) = scheduling_fixture();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    blocked_plan("Blocked by pre-tool hook".to_string()),
                ),
                (
                    "call-2".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&path)),
                ),
            ],
        );

        // The blocked call resolves immediately on its own; the executable
        // mutation is not delayed or aborted by it.
        assert_eq!(group_indexes(&groups), vec![vec![0], vec![1]]);
        assert!(matches!(groups[0][0].plan, ToolHookPlan::Block { .. }));
        assert!(matches!(groups[1][0].plan, ToolHookPlan::Execute { .. }));
    }

    #[test]
    fn undeterminable_target_stays_concurrent() {
        let (_dir, context, path, _) = scheduling_fixture();

        let groups = schedule(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan("not valid json".to_string()),
                ),
                (
                    "call-2".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments(&path)),
                ),
            ],
        );

        assert_eq!(group_indexes(&groups), vec![vec![0], vec![1]]);
    }
}
