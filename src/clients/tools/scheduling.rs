//! Tool-call scheduling for a single assistant turn.
//!
//! Tool calls in one assistant turn execute concurrently. To keep that safe
//! when the model issues multiple mutations for the same file, the scheduler
//! groups mutating calls by canonical target path: groups run concurrently
//! with each other and with non-mutating calls, while calls within a group
//! run sequentially in issue order, each seeing the previous call's effects.
//! See ADR-013.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::clients::tools::{ToolContext, ToolRegistry};
use crate::hooks::ToolHookPlan;

/// A tool call scheduled for execution, tagged with its position in the
/// model's issue order so results can be re-emitted in that order.
#[derive(Debug)]
pub struct ScheduledToolCall {
    pub index: usize,
    pub call_id: String,
    pub name: String,
    pub plan: ToolHookPlan,
}

/// Partition a turn's tool-call plans into execution groups.
///
/// Calls that would mutate the same canonical path share a group in issue
/// order; every other call — non-mutating tools, hook-blocked calls, and
/// calls whose target path cannot be determined — gets a singleton group.
/// Groups are safe to run concurrently with each other as long as members of
/// a group run sequentially in the returned order.
pub fn schedule_tool_calls(
    tools: &ToolRegistry,
    context: &ToolContext,
    tool_plans: Vec<(String, String, ToolHookPlan)>,
) -> Vec<Vec<ScheduledToolCall>> {
    let mut groups: Vec<Vec<ScheduledToolCall>> = Vec::new();
    let mut mutating_path_groups: HashMap<PathBuf, usize> = HashMap::new();

    for (index, (call_id, name, plan)) in tool_plans.into_iter().enumerate() {
        let target = match &plan {
            ToolHookPlan::Execute { arguments, .. } => {
                match tools.mutating_target(context, &name, arguments) {
                    Some(Ok(path)) => Some(path),
                    // Calls with unparseable arguments or invalid paths execute
                    // as scheduled and surface their own errors.
                    _ => None,
                }
            },
            ToolHookPlan::Block { .. } => None,
        };
        let call = ScheduledToolCall {
            index,
            call_id,
            name,
            plan,
        };

        if let Some(&group) = target
            .as_ref()
            .and_then(|path| mutating_path_groups.get(path))
        {
            groups[group].push(call);
        } else {
            if let Some(path) = target {
                mutating_path_groups.insert(path, groups.len());
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

        assert_eq!(group_indexes(&groups), vec![vec![0], vec![1]]);
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
