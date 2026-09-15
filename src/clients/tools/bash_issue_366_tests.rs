use super::*;
use crate::clients::tools::ToolContext;
use crate::clients::tools::sandbox::{SandboxConfig, SandboxPolicy};

fn minimal_sandbox_config(cwd: &std::path::Path) -> SandboxConfig {
    SandboxConfig::build_with_policy(SandboxPolicy::WorkspaceWrite, cwd, &[], &[], &[], &[])
}

fn outside_tempdir() -> tempfile::TempDir {
    // The minimal config intentionally omits the sandbox's broad temp grants,
    // while `/tmp` remains writable to the test harness for fixture setup.
    tempfile::TempDir::new_in("/tmp").expect("outside fixture")
}

#[test]
fn denied_write_targets_include_absent_paths_and_operation() {
    let cwd = tempfile::tempdir().expect("workspace fixture");
    let outside = outside_tempdir();
    let target = outside.path().join("new-scratch.txt");
    let config = minimal_sandbox_config(cwd.path());

    let command = format!("touch {}", target.display());
    let denials = denied_paths_in_command(&command, cwd.path(), &config);

    assert_eq!(denials, vec![format!("  - {} (write)", target.display())]);
    let refs = denied_path_refs_in_command(&command, cwd.path(), &config);
    assert_eq!(
        refs[0].permission_label(),
        format!("sandbox: write {}", target.display())
    );
}

#[test]
fn denied_redirection_target_is_a_write_operation() {
    let cwd = tempfile::tempdir().expect("workspace fixture");
    let outside = outside_tempdir();
    let target = outside.path().join("redirected.txt");
    let config = minimal_sandbox_config(cwd.path());

    let command = format!("printf data > {}", target.display());
    let denials = denied_paths_in_command(&command, cwd.path(), &config);

    assert_eq!(denials, vec![format!("  - {} (write)", target.display())]);
}

#[test]
fn read_only_workspace_write_is_still_a_denied_path() {
    let cwd = tempfile::tempdir().expect("workspace fixture");
    let target = cwd.path().join("read-only-output.txt");
    let config =
        SandboxConfig::build_with_policy(SandboxPolicy::ReadOnly, cwd.path(), &[], &[], &[], &[]);

    let command = format!("touch {}", target.display());
    let refs = denied_path_refs_in_command(&command, cwd.path(), &config);

    assert_eq!(
        refs.iter()
            .map(SandboxPathRef::permission_label)
            .collect::<Vec<_>>(),
        vec![format!("sandbox: write {}", target.display())]
    );
}

#[test]
fn copy_sources_are_reads_and_destination_is_write() {
    let cwd = tempfile::tempdir().expect("workspace fixture");
    let outside = outside_tempdir();
    let source = outside.path().join("source.txt");
    let destination = outside.path().join("destination.txt");
    std::fs::write(&source, "source").expect("source fixture");
    let config = minimal_sandbox_config(cwd.path());

    let command = format!("cp {} {}", source.display(), destination.display());
    let refs = denied_path_refs_in_command(&command, cwd.path(), &config);

    assert_eq!(
        refs.iter()
            .map(SandboxPathRef::permission_label)
            .collect::<Vec<_>>(),
        vec![
            format!("sandbox: read {}", source.display()),
            format!("sandbox: write {}", destination.display()),
        ]
    );
}

#[test]
fn permission_label_keeps_sandbox_source_distinct() {
    let denial = SandboxPathRef {
        path: std::path::PathBuf::from("/outside/input.txt"),
        operation: SandboxPathOperation::Read,
    };
    assert_eq!(
        denial.permission_label(),
        "sandbox: read /outside/input.txt"
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn sandbox_write_denial_reaches_tool_result_permission_denials() {
    #[cfg(target_os = "macos")]
    if super::super::sandbox::is_sandbox_disabled()
        || !super::super::sandbox::can_enforce_platform_sandbox()
    {
        return;
    }

    let account_home = temp_env::with_var("HOME", None::<&str>, dirs::home_dir)
        .expect("OS account home must be available");
    let outside = tempfile::TempDir::new_in(account_home).expect("outside fixture");
    let target = outside.path().join("denied-write.txt");
    let read_target = outside.path().join("denied-read.txt");
    std::fs::write(&read_target, "outside secret").expect("read fixture");
    let mut context = ToolContext::from_current_process();
    context.judge = Some(bypassed_judge_context());
    let args = serde_json::json!({
        "command": format!("touch {}", target.display()),
    })
    .to_string();

    let result = execute_bash(&context, &args)
        .await
        .expect("sandboxed command should return a tool result");

    assert!(result.output.contains("[Sandbox restriction]"));
    assert_eq!(
        result.permission_denials,
        vec![format!("sandbox: write {}", target.display())]
    );
    assert!(
        !target.exists(),
        "sandbox denial must not create the target"
    );

    let read_args = serde_json::json!({
        "command": format!("cat {}", read_target.display()),
    })
    .to_string();
    let read_result = execute_bash(&context, &read_args)
        .await
        .expect("sandboxed read should return a tool result");

    assert!(read_result.output.contains("[Sandbox restriction]"));
    assert_eq!(
        read_result.permission_denials,
        vec![format!("sandbox: read {}", read_target.display())]
    );
}
