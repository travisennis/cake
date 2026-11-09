use super::*;
use crate::clients::tools::{resolve_path_for_write_scheduling, validate_path_for_write};

fn workspace_grants(
    policy: SandboxPolicy,
    home: &Path,
    cwd: &Path,
    temp_dirs: &[PathBuf],
    settings_dirs: &[PathBuf],
) -> PathGrants {
    PathGrants::build(policy, cwd, temp_dirs, &[], settings_dirs, &[], Some(home))
}

#[test]
fn in_process_projection_keeps_toolchain_caches_read_only() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let settings = tempfile::tempdir().unwrap();
    let bun = home.path().join(".bun");
    std::fs::create_dir_all(bun.join("install/cache")).unwrap();

    let grants = workspace_grants(
        SandboxPolicy::WorkspaceWrite,
        home.path(),
        cwd.path(),
        &[],
        &[settings.path().to_path_buf()],
    );

    assert!(
        grants.read_write.contains(&cwd.path().to_path_buf()),
        "the workspace must stay read-write"
    );
    assert!(
        grants.read_write.contains(&settings.path().to_path_buf()),
        "settings directories must stay read-write"
    );
    assert!(
        grants.read_only.contains(&bun),
        "the toolchain cache must be readable in-process"
    );
    assert!(
        !grants.read_write.contains(&bun),
        "the toolchain cache must not be writable in-process"
    );

    let config = SandboxConfig::from_grants(&grants);
    assert!(
        config.writable.contains(&bun),
        "Bash must keep its toolchain-cache write grant"
    );
    assert!(
        !config.read_execute.contains(&bun),
        "the Bash projection must not duplicate the writable toolchain grant"
    );
}

#[test]
fn read_only_policy_demotes_workspace_but_keeps_temp_writable() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let temp_dirs = vec![temp.path().to_path_buf()];
    let temp_path = temp.path().to_path_buf();
    let cwd_path = cwd.path().to_path_buf();

    let grants = workspace_grants(
        SandboxPolicy::ReadOnly,
        home.path(),
        cwd.path(),
        &temp_dirs,
        &[],
    );

    assert!(
        grants.read_write.contains(&temp_path),
        "temp directories must stay writable"
    );
    assert!(
        !grants.read_write.contains(&cwd_path),
        "the workspace must be demoted under the read-only policy"
    );
    assert!(
        grants.read_only.contains(&cwd_path),
        "the demoted workspace must remain readable"
    );
    assert!(
        grants.sandbox_writable.is_empty(),
        "Bash must not keep toolchain-cache writes under the read-only policy"
    );

    let config = SandboxConfig::from_grants(&grants);
    assert!(config.writable.contains(&temp_path));
    assert!(!config.writable.contains(&cwd_path));
    assert!(config.read_execute.contains(&cwd_path));
}

#[test]
fn denied_paths_match_files_exactly_and_directories_by_subtree() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
    std::fs::create_dir_all(home.path().join(".cargo/registry/src/crate")).unwrap();
    std::fs::write(home.path().join(".ssh/id_ed25519"), "synthetic-key").unwrap();
    std::fs::write(home.path().join(".npmrc"), "synthetic-token").unwrap();
    std::fs::write(home.path().join(".npmrc.backup"), "backup").unwrap();
    std::fs::write(
        home.path().join(".cargo/credentials.toml"),
        "synthetic-credential",
    )
    .unwrap();
    std::fs::write(
        home.path().join(".cargo/registry/src/crate/lib.rs"),
        "pub fn source() {}",
    )
    .unwrap();

    let grants = workspace_grants(
        SandboxPolicy::WorkspaceWrite,
        home.path(),
        cwd.path(),
        &[],
        &[home.path().to_path_buf()],
    );

    // The ancestor grant is present, but the deny list still matches it.
    assert!(grants.read_write.contains(&home.path().to_path_buf()));
    assert!(grants.is_denied(&home.path().join(".npmrc")));
    assert!(grants.is_denied(&home.path().join(".ssh/id_ed25519")));
    assert!(grants.is_denied(&home.path().join(".cargo/credentials.toml")));

    assert!(
        !grants.is_denied(&home.path().join(".npmrc.backup")),
        "a file deny entry must not deny a sibling"
    );
    assert!(
        !grants.is_denied(&home.path().join(".cargo/registry/src/crate/lib.rs")),
        "a file deny entry must not deny the toolchain subtree"
    );

    // The OS projection deliberately ignores the in-process deny list.
    let config = SandboxConfig::from_grants(&grants);
    assert!(config.is_path_allowed(&home.path().join(".npmrc")));
}

#[cfg(unix)]
#[test]
fn denied_symlink_is_canonicalized() {
    use std::os::unix::fs::symlink;

    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
    let secret = home.path().join(".ssh/id_ed25519");
    std::fs::write(&secret, "synthetic-key").unwrap();

    let file_link = cwd.path().join("secret-link");
    symlink(&secret, &file_link).unwrap();
    let directory_link = cwd.path().join("ssh-link");
    symlink(home.path().join(".ssh"), &directory_link).unwrap();

    let grants = workspace_grants(
        SandboxPolicy::WorkspaceWrite,
        home.path(),
        cwd.path(),
        &[],
        &[],
    );

    assert!(
        grants.is_denied(&file_link),
        "a symlink to a denied file must fail closed"
    );
    assert!(
        grants.is_denied(&directory_link.join("id_ed25519")),
        "a path through a symlinked denied directory must fail closed"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_os_only_reads_stay_out_of_the_in_process_projection() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let grants = workspace_grants(
        SandboxPolicy::WorkspaceWrite,
        home.path(),
        cwd.path(),
        &[],
        &[],
    );

    for path in [Path::new("/proc"), Path::new("/dev"), Path::new("/sys")] {
        assert!(
            grants.sandbox_only_read.iter().any(|grant| grant == path),
            "{} must stay in the OS-only projection",
            path.display()
        );
        assert!(
            !grants.read_only.iter().any(|grant| grant == path),
            "{} must not enter the in-process projection",
            path.display()
        );
    }
    let config = SandboxConfig::from_grants(&grants);
    assert!(
        config
            .read_execute
            .iter()
            .any(|grant| grant == Path::new("/proc"))
    );
}

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
        "cat \"$1\" && ! printf changed >> \"$1\"",
        "cake-read-only-grant-test",
        nested.to_str().unwrap(),
    ]);
    let _guard = strategy.apply(&mut command, &config).unwrap();
    let output = command.output().await.unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read_to_string(&nested).unwrap(), "first");
}
