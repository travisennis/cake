use super::*;
use crate::clients::tools::{resolve_path_for_write_scheduling, validate_path_for_write};

#[tokio::test]
async fn provisioned_grant_allows_writes_without_granting_parent_or_sibling() {
    if is_sandbox_disabled() {
        return;
    }
    #[cfg(target_os = "macos")]
    if !can_enforce_platform_sandbox() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let target = root.path().join("absent/parent/state");
    let grants = crate::cli::prepare_writable_dirs(
        &[],
        &[target.to_str().unwrap().to_owned()],
        SandboxPolicy::WorkspaceWrite,
    )
    .unwrap();
    let mut context = ToolContext::with_temp_dirs(workspace, vec![], vec![], vec![], grants);
    let nested = target.join("new/first.txt");
    let parent = target.parent().unwrap().join("parent.txt");
    let sibling = target.with_file_name("sibling");
    assert!(resolve_path_for_write_scheduling(&context, nested.to_str().unwrap()).is_ok());
    assert!(resolve_path_for_write_scheduling(&context, parent.to_str().unwrap()).is_err());
    assert!(resolve_path_for_write_scheduling(&context, sibling.to_str().unwrap()).is_err());

    // Pin HOME during config construction to avoid ambient toolchain grants.
    let config = temp_env::with_var("HOME", Some(root.path().join("home")), || {
        SandboxConfig::build(&context)
    });
    let strategy = detect_platform()
        .unwrap()
        .expect("platform sandbox required");
    let mut command = tokio::process::Command::new("bash");
    command.current_dir(&context.cwd).args([
        "-c",
        "mkdir -p \"$1/new\" && printf first > \"$1/new/first.txt\" && ! touch \"$2\" && ! mkdir \"$3\"",
        "cake-grant-test",
        target.to_str().unwrap(),
        parent.to_str().unwrap(),
        sibling.to_str().unwrap(),
    ]);
    let _guard = strategy.apply(&mut command, &config).unwrap();
    let output = command.output().await.unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read_to_string(&nested).unwrap(), "first");
    assert!(!parent.exists());
    assert!(!sibling.exists());
    assert!(validate_path_for_write(&context, nested.to_str().unwrap()).is_ok());

    context.sandbox_policy = SandboxPolicy::ReadOnly;
    let config = temp_env::with_var("HOME", Some(root.path().join("home")), || {
        SandboxConfig::build(&context)
    });
    let mut command = tokio::process::Command::new("bash");
    command.current_dir(&context.cwd).args([
        "-c",
        "cat \"$1\" && ! touch \"$1\"",
        "cake-read-only-grant-test",
        nested.to_str().unwrap(),
    ]);
    let _guard = strategy.apply(&mut command, &config).unwrap();
    let output = command.output().await.unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"first");
}
