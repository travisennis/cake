//! Duplicate mutation guard for tool execution.
//!
//! Within a single assistant turn, the model may issue multiple tool calls. This
//! module prevents duplicate (or overlapping) mutations to the same file path
//! within one turn, rejecting the duplicate with a clear error message so the
//! model can react and issue a new single mutation after re-reading the file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::clients::tools::{ToolContext, ToolRegistry};
use crate::hooks::ToolHookPlan;

/// The resolved plan for a single tool call within an assistant turn.
#[derive(Debug)]
pub enum ScheduledToolPlan {
    /// Pass through to normal hook processing (execute or block).
    Hook(ToolHookPlan),
    /// The tool call was rejected because a mutation for the same file was
    /// already scheduled in this turn.
    RejectedDuplicateMutation { output: String },
}

/// Examine a batch of tool-call plans and reject any that would mutate a file
/// that already has a scheduled mutation in the same turn.
///
/// Non-mutating tools and tools whose target path cannot be determined are
/// passed through unchanged. Only the first mutation targeting a given file
/// survives; subsequent mutations for the same canonical path are replaced
/// with [`ScheduledToolPlan::RejectedDuplicateMutation`].
pub fn reject_duplicate_mutating_tool_calls(
    tools: &ToolRegistry,
    context: &ToolContext,
    tool_plans: Vec<(String, String, ToolHookPlan)>,
) -> Vec<(String, String, ScheduledToolPlan)> {
    let mut seen_mutating_paths: HashMap<PathBuf, String> = HashMap::new();

    tool_plans
        .into_iter()
        .map(|(call_id, name, plan)| {
            let ToolHookPlan::Execute { arguments, .. } = &plan else {
                return (call_id, name, ScheduledToolPlan::Hook(plan));
            };

            let Some(Ok(path)) = tools.mutating_target(context, &name, arguments) else {
                return (call_id, name, ScheduledToolPlan::Hook(plan));
            };

            if let Some(first_tool_name) = seen_mutating_paths.get(&path) {
                let output = duplicate_mutation_rejection_output(&name, first_tool_name, &path);
                return (
                    call_id,
                    name,
                    ScheduledToolPlan::RejectedDuplicateMutation { output },
                );
            }

            seen_mutating_paths.insert(path, name.clone());
            (call_id, name, ScheduledToolPlan::Hook(plan))
        })
        .collect()
}

/// Build the user-facing error message for a rejected duplicate mutation.
fn duplicate_mutation_rejection_output(
    tool_name: &str,
    first_tool_name: &str,
    path: &Path,
) -> String {
    format!(
        "Error: Rejected this {tool_name} call because another {first_tool_name} call for the same file was already issued in this assistant turn: {}. Wait for the previous tool result, re-read the file, and then issue one follow-up Edit or Write call if more changes are needed.",
        path.display()
    )
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

    fn duplicate_guard_fixture() -> (TempDir, ToolContext, PathBuf, PathBuf) {
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

    fn reject_duplicate_plans(
        context: &ToolContext,
        tool_plans: Vec<(String, String, ToolHookPlan)>,
    ) -> Vec<(String, String, ScheduledToolPlan)> {
        reject_duplicate_mutating_tool_calls(&default_tool_registry(), context, tool_plans)
    }

    #[test]
    fn duplicate_guard_rejects_second_edit_for_same_file() {
        let (_dir, context, path, _) = duplicate_guard_fixture();
        let edit_arguments = serde_json::json!({
            "path": path,
            "edits": [{ "old_text": "first", "new_text": "updated" }]
        })
        .to_string();

        let plans = reject_duplicate_plans(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments.clone()),
                ),
                (
                    "call-2".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments),
                ),
            ],
        );

        assert!(matches!(plans[0].2, ScheduledToolPlan::Hook(_)));
        assert!(
            matches!(&plans[1].2, ScheduledToolPlan::RejectedDuplicateMutation { output } if
                output.contains("Rejected this Edit call")
                    && output.contains("same file")
                    && output.contains("re-read the file"))
        );
    }

    #[test]
    fn duplicate_guard_rejects_write_after_edit_for_same_file() {
        let (_dir, context, path, _) = duplicate_guard_fixture();
        let edit_arguments = serde_json::json!({
            "path": path,
            "edits": [{ "old_text": "first", "new_text": "updated" }]
        })
        .to_string();
        let write_arguments = serde_json::json!({
            "path": path,
            "content": "replacement"
        })
        .to_string();

        let plans = reject_duplicate_plans(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan(edit_arguments),
                ),
                (
                    "call-2".to_string(),
                    "Write".to_string(),
                    execute_plan(write_arguments),
                ),
            ],
        );

        assert!(matches!(plans[0].2, ScheduledToolPlan::Hook(_)));
        assert!(
            matches!(&plans[1].2, ScheduledToolPlan::RejectedDuplicateMutation { output } if
                output.contains("Rejected this Write call")
                    && output.contains("another Edit call"))
        );
    }

    #[test]
    fn duplicate_guard_rejects_relative_and_absolute_paths_for_same_file() {
        let (_dir, context, path, _) = duplicate_guard_fixture();
        let current_dir = std::env::current_dir().unwrap();
        let relative_path = path.strip_prefix(current_dir).unwrap();
        let absolute_arguments = serde_json::json!({
            "path": path,
            "edits": [{ "old_text": "first", "new_text": "updated" }]
        })
        .to_string();
        let relative_arguments = serde_json::json!({
            "path": relative_path,
            "content": "replacement"
        })
        .to_string();

        let plans = reject_duplicate_plans(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan(absolute_arguments),
                ),
                (
                    "call-2".to_string(),
                    "Write".to_string(),
                    execute_plan(relative_arguments),
                ),
            ],
        );

        assert!(matches!(plans[0].2, ScheduledToolPlan::Hook(_)));
        assert!(
            matches!(&plans[1].2, ScheduledToolPlan::RejectedDuplicateMutation { output } if
                output.contains("Rejected this Write call")
                    && output.contains("same file"))
        );
    }

    #[test]
    fn duplicate_guard_allows_mutations_to_different_files() {
        let (_dir, context, first_path, second_path) = duplicate_guard_fixture();
        let first_arguments = serde_json::json!({
            "path": first_path,
            "edits": [{ "old_text": "first", "new_text": "updated" }]
        })
        .to_string();
        let second_arguments = serde_json::json!({
            "path": second_path,
            "content": "replacement"
        })
        .to_string();

        let plans = reject_duplicate_plans(
            &context,
            vec![
                (
                    "call-1".to_string(),
                    "Edit".to_string(),
                    execute_plan(first_arguments),
                ),
                (
                    "call-2".to_string(),
                    "Write".to_string(),
                    execute_plan(second_arguments),
                ),
            ],
        );

        assert!(
            plans
                .iter()
                .all(|(_, _, plan)| matches!(plan, ScheduledToolPlan::Hook(_)))
        );
    }

    #[test]
    fn duplicate_guard_allows_repeated_reads_for_same_file() {
        let (_dir, context, path, _) = duplicate_guard_fixture();
        let read_arguments = serde_json::json!({ "path": path }).to_string();

        let plans = reject_duplicate_plans(
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

        assert!(
            plans
                .iter()
                .all(|(_, _, plan)| matches!(plan, ScheduledToolPlan::Hook(_)))
        );
    }
}
