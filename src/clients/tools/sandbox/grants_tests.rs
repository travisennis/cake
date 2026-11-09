use super::*;
use crate::clients::tools::{
    default_tool_registry, edit::execute_edit, read::execute_read, read_image::execute_read_image,
    resolve_path_for_write_scheduling, validate_path, validate_path_for_write,
    write::execute_write,
};
use std::collections::HashSet;

fn workspace_grants(
    policy: SandboxPolicy,
    home: &Path,
    cwd: &Path,
    temp_dirs: &[PathBuf],
    settings_dirs: &[PathBuf],
) -> PathGrants {
    PathGrants::build(policy, cwd, temp_dirs, &[], settings_dirs, &[], Some(home))
}

fn context_with_home(home: &Path, cwd: &Path, settings_dirs: &[PathBuf]) -> ToolContext {
    temp_env::with_var("HOME", Some(home), || {
        ToolContext::with_temp_dirs(
            cwd.to_path_buf(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            settings_dirs.to_vec(),
        )
    })
}

fn assert_credential_denial(error: &str, path: &Path, secret: &str) {
    assert!(error.contains("credential-path policy"), "{error}");
    assert!(error.contains(&path.display().to_string()), "{error}");
    assert!(
        !error.contains(secret),
        "the denial must not echo synthetic secret contents: {error}"
    );
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
fn config_read_and_user_skill_grants_project_to_both_layers() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let config = home.path().join(".config/cake");
    let skills = home.path().join(".agents/skills");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&skills).unwrap();

    let mut context = temp_env::with_var("HOME", Some(home.path()), || {
        ToolContext::with_temp_dirs(
            cwd.path().to_path_buf(),
            Vec::new(),
            vec![config.clone()],
            Vec::new(),
            vec![skills.clone()],
        )
    });
    let grants = context.path_grants();
    assert!(
        grants.read_only.contains(&config),
        "the global config directory must be read-only in-process"
    );
    assert!(
        !grants.read_write.contains(&config),
        "the global config directory must not be writable in-process"
    );
    assert!(
        grants.read_write.contains(&skills),
        "the user skill root must be writable in-process"
    );

    let os = SandboxConfig::from_grants(grants);
    assert!(
        os.read_execute.contains(&config),
        "Bash must read the global config directory"
    );
    assert!(
        !os.writable.contains(&config),
        "Bash must not write the global config directory"
    );
    assert!(
        os.writable.contains(&skills),
        "Bash must write the user skill root"
    );

    let config_file = config.join("settings.toml");
    std::fs::write(&config_file, "default_model = \"test\"\n").unwrap();
    validate_path(&context, config_file.to_str().unwrap()).unwrap();
    let error = validate_path_for_write(&context, config_file.to_str().unwrap()).unwrap_err();
    assert!(error.contains("read-only"), "{error}");

    let skill_file = skills.join("example/SKILL.md");
    std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
    std::fs::write(&skill_file, "---\nname: example\n---\n").unwrap();
    validate_path_for_write(&context, skill_file.to_str().unwrap()).unwrap();

    context.set_sandbox_policy(SandboxPolicy::ReadOnly);
    let grants = context.path_grants();
    assert!(
        grants.read_only.contains(&skills),
        "read-only must demote the user skill root"
    );
    assert!(
        !grants.read_write.contains(&skills),
        "read-only must not keep the user skill root writable"
    );
    let error = validate_path_for_write(&context, skill_file.to_str().unwrap()).unwrap_err();
    assert!(error.contains("read-only"), "{error}");
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
fn read_only_policy_demotes_settings_and_hides_mutating_tools() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let settings = tempfile::tempdir().unwrap();
    let settings_file = settings.path().join("settings.txt");
    let temp_file = temp.path().join("temp.txt");
    std::fs::write(&settings_file, "settings").unwrap();
    std::fs::write(&temp_file, "temp").unwrap();
    let settings_path = settings.path().to_path_buf();
    let temp_path = temp.path().to_path_buf();

    let mut context = temp_env::with_var("HOME", Some(home.path()), || {
        ToolContext::with_temp_dirs(
            cwd.path().to_path_buf(),
            vec![temp_path.clone()],
            vec![],
            vec![],
            vec![settings_path.clone()],
        )
    });
    temp_env::with_var("HOME", Some(home.path()), || {
        context.set_sandbox_policy(SandboxPolicy::ReadOnly);
    });

    let grants = context.path_grants();
    assert!(grants.read_write.contains(&temp_path));
    assert!(!grants.read_write.contains(&settings_path));
    assert!(grants.read_only.contains(&settings_path));
    let error = validate_path_for_write(&context, settings_file.to_str().unwrap()).unwrap_err();
    assert!(error.contains("read-only"), "{error}");
    assert!(validate_path_for_write(&context, temp_file.to_str().unwrap()).is_ok());

    let mut registry = default_tool_registry();
    registry.retain_read_safe_tools();
    let names = registry.names();
    assert!(names.iter().any(|name| name == "Read"));
    assert!(!names.iter().any(|name| name == "Edit" || name == "Write"));
}

#[test]
fn cached_grants_ignore_later_home_changes() {
    let home = tempfile::tempdir().unwrap();
    let other_home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".bun/install/cache")).unwrap();

    let context = temp_env::with_var("HOME", Some(home.path()), || {
        ToolContext::with_temp_dirs(cwd.path().to_path_buf(), vec![], vec![], vec![], vec![])
    });

    let config = temp_env::with_var("HOME", Some(other_home.path()), || {
        SandboxConfig::build(&context)
    });

    assert!(
        config.writable.contains(&home.path().join(".bun")),
        "the config must keep the home captured at context construction"
    );
    assert!(
        !config.writable.contains(&other_home.path().join(".bun")),
        "a later HOME change must not alter the cached grants"
    );
}

#[test]
fn policy_change_rebuilds_cached_grants() {
    let cwd = tempfile::tempdir().unwrap();
    let cwd_path = cwd.path().to_path_buf();
    let mut context = ToolContext::with_temp_dirs(cwd_path.clone(), vec![], vec![], vec![], vec![]);

    let config = SandboxConfig::build(&context);
    assert!(config.writable.contains(&cwd_path));

    context.set_sandbox_policy(SandboxPolicy::ReadOnly);
    let config = SandboxConfig::build(&context);
    assert!(
        !config.writable.contains(&cwd_path),
        "the read-only policy must demote the cached workspace grant"
    );
    assert!(
        config.read_execute.contains(&cwd_path),
        "the demoted workspace must remain readable"
    );
}

#[test]
fn in_process_grants_validate_and_project_to_matching_os_sets() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let additional = tempfile::tempdir().unwrap();
    let settings = tempfile::tempdir().unwrap();
    let skill = tempfile::tempdir().unwrap();
    let cargo_home = home.path().join(".cargo");
    let rustup_home = home.path().join(".rustup");
    let source = cargo_home.join("registry/src/crate/lib.rs");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::create_dir_all(rustup_home.join("toolchains/stable")).unwrap();
    std::fs::create_dir_all(home.path().join(".bun/install/cache")).unwrap();
    std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
    std::fs::write(&source, "pub fn source() {}").unwrap();
    std::fs::write(home.path().join(".ssh/id_ed25519"), "synthetic-key").unwrap();
    std::fs::write(home.path().join(".npmrc"), "synthetic-token").unwrap();

    let context = temp_env::with_var("HOME", Some(home.path()), || {
        temp_env::with_var("CARGO_HOME", Some(&cargo_home), || {
            temp_env::with_var("RUSTUP_HOME", Some(&rustup_home), || {
                ToolContext::with_temp_dirs(
                    cwd.path().to_path_buf(),
                    vec![temp.path().to_path_buf()],
                    vec![additional.path().to_path_buf()],
                    vec![skill.path().to_path_buf()],
                    vec![settings.path().to_path_buf()],
                )
            })
        })
    });

    let grants = context.path_grants();
    let mut checked = 0;
    for path in grants.read_write.iter().chain(grants.read_only.iter()) {
        if !path.exists() || grants.is_denied(path) {
            continue;
        }
        let path_str = path.to_str().unwrap();
        validate_path(&context, path_str).unwrap_or_else(|error| {
            panic!("in-process grant {} must validate: {error}", path.display())
        });
        if grants.read_write.contains(path) {
            validate_path_for_write(&context, path_str).unwrap_or_else(|error| {
                panic!(
                    "read-write grant {} must be writable: {error}",
                    path.display()
                )
            });
        }
        checked += 1;
    }
    assert!(checked > 0, "the parity test must inspect real grants");

    let config = SandboxConfig::build(&context);
    let expected_writable: HashSet<PathBuf> = grants
        .read_write
        .iter()
        .chain(grants.sandbox_writable.iter())
        .cloned()
        .collect();
    let actual_writable: HashSet<PathBuf> = config.writable.iter().cloned().collect();
    assert_eq!(actual_writable, expected_writable);

    let sandbox_writable: HashSet<PathBuf> = grants.sandbox_writable.iter().cloned().collect();
    let expected_read_execute: HashSet<PathBuf> = grants
        .read_only
        .iter()
        .filter(|path| !sandbox_writable.contains(*path))
        .chain(grants.sandbox_only_read.iter())
        .cloned()
        .collect();
    let actual_read_execute: HashSet<PathBuf> = config.read_execute.iter().cloned().collect();
    assert_eq!(actual_read_execute, expected_read_execute);
    assert_eq!(config.user_grants, grants.user_grants);
}

#[test]
fn validator_denies_credentials_before_a_granted_home() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let secret = home.path().join(".npmrc");
    std::fs::write(&secret, "synthetic-token").unwrap();

    let context = temp_env::with_var("HOME", Some(home.path()), || {
        ToolContext::with_temp_dirs(
            cwd.path().to_path_buf(),
            vec![],
            vec![],
            vec![],
            vec![home.path().to_path_buf()],
        )
    });

    let error = validate_path(&context, secret.to_str().unwrap()).unwrap_err();
    assert!(error.contains("credential-path policy"), "{error}");
    assert!(error.contains(secret.to_str().unwrap()), "{error}");
    assert!(!error.contains("synthetic-token"), "{error}");

    let error = validate_path_for_write(&context, secret.to_str().unwrap()).unwrap_err();
    assert!(error.contains("credential-path policy"), "{error}");
    assert!(!error.contains("synthetic-token"), "{error}");
}

#[test]
fn validator_reads_toolchain_source_but_rejects_writes() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let cargo_home = home.path().join(".cargo");
    let source = cargo_home.join("registry/src/crate/lib.rs");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, "pub fn source() {}").unwrap();

    let context = temp_env::with_var("HOME", Some(home.path()), || {
        temp_env::with_var("CARGO_HOME", Some(&cargo_home), || {
            ToolContext::with_temp_dirs(cwd.path().to_path_buf(), vec![], vec![], vec![], vec![])
        })
    });

    assert!(
        validate_path(&context, source.to_str().unwrap()).is_ok(),
        "toolchain source must be readable through the shared grants"
    );
    let error = validate_path_for_write(&context, source.to_str().unwrap()).unwrap_err();
    assert!(error.contains("read-only"), "{error}");
}

#[test]
fn file_tools_use_toolchain_grants_without_extra_dirs() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let cargo_home = home.path().join(".cargo");
    let rustup_home = home.path().join(".rustup");
    let sources = [
        cargo_home.join("registry/src/crate/lib.rs"),
        rustup_home.join("toolchains/stable/lib.rs"),
    ];
    for source in &sources {
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(source, "pub fn toolchain() {}\n").unwrap();
    }

    let context = temp_env::with_var("HOME", Some(home.path()), || {
        temp_env::with_var("CARGO_HOME", Some(&cargo_home), || {
            temp_env::with_var("RUSTUP_HOME", Some(&rustup_home), || {
                ToolContext::with_temp_dirs(
                    cwd.path().to_path_buf(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            })
        })
    });

    for source in &sources {
        let path = source.to_str().unwrap();
        let read_args = serde_json::json!({ "path": path }).to_string();
        let output = execute_read(&context, &read_args).unwrap();
        assert!(
            output.output.contains("pub fn toolchain() {}"),
            "Read must expose toolchain source: {}",
            output.output
        );

        let error = validate_path_for_write(&context, path).unwrap_err();
        assert!(error.contains("read-only"), "{error}");

        let before = std::fs::read(source).unwrap();
        let write_args = serde_json::json!({ "path": path, "content": "changed" }).to_string();
        let error = execute_write(&context, &write_args).unwrap_err();
        assert!(error.contains("read-only"), "{error}");
        assert_eq!(std::fs::read(source).unwrap(), before);

        let edit_args = serde_json::json!({
            "path": path,
            "edits": [{ "old_text": "pub fn toolchain() {}", "new_text": "changed" }]
        })
        .to_string();
        let error = execute_edit(&context, &edit_args).unwrap_err();
        assert!(error.contains("read-only"), "{error}");
        assert_eq!(std::fs::read(source).unwrap(), before);
    }
}

#[test]
fn credential_denylist_wins_over_granted_home_for_file_tools() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let credentials = [
        (home.path().join(".npmrc"), "synthetic-npm-secret"),
        (
            home.path().join(".cargo/credentials.toml"),
            "synthetic-cargo-secret",
        ),
        (
            home.path().join(".config/gh/hosts.yml"),
            "synthetic-gh-secret",
        ),
        (home.path().join(".ssh/id_ed25519"), "synthetic-ssh-secret"),
        (home.path().join(".netrc"), "synthetic-netrc-secret"),
    ];
    for (path, secret) in &credentials {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, secret).unwrap();
    }

    let context = context_with_home(home.path(), cwd.path(), &[home.path().to_path_buf()]);

    for (path, secret) in &credentials {
        let path_str = path.to_str().unwrap();
        let read_args = serde_json::json!({ "path": path_str }).to_string();
        let error = execute_read(&context, &read_args).unwrap_err();
        assert_credential_denial(&error, path, secret);

        let error = execute_read_image(&context, &read_args).unwrap_err();
        assert_credential_denial(&error, path, secret);

        let error = validate_path(&context, path_str).unwrap_err();
        assert_credential_denial(&error, path, secret);

        let before = std::fs::read(path).unwrap();
        let write_args = serde_json::json!({ "path": path_str, "content": "changed" }).to_string();
        let error = execute_write(&context, &write_args).unwrap_err();
        assert_credential_denial(&error, path, secret);
        assert_eq!(std::fs::read(path).unwrap(), before);

        let edit_args = serde_json::json!({
            "path": path_str,
            "edits": [{ "old_text": secret, "new_text": "changed" }]
        })
        .to_string();
        let error = execute_edit(&context, &edit_args).unwrap_err();
        assert_credential_denial(&error, path, secret);
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

#[test]
fn file_deny_entry_does_not_hide_a_sibling() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let secret = home.path().join(".npmrc");
    let sibling = home.path().join(".npmrc.backup");
    std::fs::write(&secret, "synthetic-token").unwrap();
    std::fs::write(&sibling, "synthetic-backup").unwrap();

    let context = context_with_home(home.path(), cwd.path(), &[home.path().to_path_buf()]);
    let read_args = serde_json::json!({ "path": sibling.to_str().unwrap() }).to_string();
    let output = execute_read(&context, &read_args).unwrap();
    assert!(
        output.output.contains("synthetic-backup"),
        "a file deny entry must not hide a sibling: {}",
        output.output
    );

    let read_args = serde_json::json!({ "path": secret.to_str().unwrap() }).to_string();
    let error = execute_read(&context, &read_args).unwrap_err();
    assert_credential_denial(&error, &secret, "synthetic-token");
}

#[cfg(unix)]
#[test]
fn read_rejects_symlinks_to_denied_files_from_cwd_and_granted_tree() {
    use std::os::unix::fs::symlink;

    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let granted = home.path().join("granted");
    std::fs::create_dir_all(&granted).unwrap();
    std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
    let secret = home.path().join(".ssh/id_ed25519");
    std::fs::write(&secret, "synthetic-key").unwrap();

    let links = [
        cwd.path().join("cwd-secret-link"),
        granted.join("granted-secret-link"),
    ];
    for link in &links {
        symlink(&secret, link).unwrap();
    }

    let context = context_with_home(home.path(), cwd.path(), &[granted]);
    for link in &links {
        let args = serde_json::json!({ "path": link.to_str().unwrap() }).to_string();
        let error = execute_read(&context, &args).unwrap_err();
        assert_credential_denial(&error, link, "synthetic-key");

        let error = validate_path(&context, link.to_str().unwrap()).unwrap_err();
        assert_credential_denial(&error, link, "synthetic-key");
    }
}

#[cfg(unix)]
#[test]
fn symlink_to_a_path_outside_every_grant_is_rejected() {
    use std::os::unix::fs::symlink;

    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir_in("/tmp").unwrap();
    let target = outside.path().join("outside.txt");
    std::fs::write(&target, "outside-contents").unwrap();
    let link = cwd.path().join("outside-link");
    symlink(&target, &link).unwrap();

    let context = context_with_home(home.path(), cwd.path(), &[]);
    let error = validate_path(&context, link.to_str().unwrap()).unwrap_err();
    assert!(error.contains("outside the working directory"), "{error}");

    let args = serde_json::json!({ "path": link.to_str().unwrap() }).to_string();
    let error = execute_read(&context, &args).unwrap_err();
    assert!(error.contains("outside the working directory"), "{error}");
    assert!(!error.contains("outside-contents"), "{error}");
}

#[test]
fn prospective_writes_under_denied_paths_are_rejected() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let denied_dir = home.path().join(".ssh");
    std::fs::create_dir_all(&denied_dir).unwrap();

    let context = temp_env::with_var("HOME", Some(home.path()), || {
        ToolContext::with_temp_dirs(
            cwd.path().to_path_buf(),
            vec![],
            vec![],
            vec![],
            vec![home.path().to_path_buf()],
        )
    });

    for target in [denied_dir.join("new-key"), home.path().join(".npmrc")] {
        let error =
            resolve_path_for_write_scheduling(&context, target.to_str().unwrap()).unwrap_err();
        assert!(error.contains("credential-path policy"), "{error}");
        assert!(!target.exists(), "a denied write must create nothing");

        let args = serde_json::json!({
            "path": target.to_str().unwrap(),
            "content": "changed"
        })
        .to_string();
        let error = execute_write(&context, &args).unwrap_err();
        assert!(error.contains("credential-path policy"), "{error}");
        assert!(!target.exists(), "a denied Write must create nothing");
    }
}

#[cfg(unix)]
#[test]
fn prospective_write_through_dangling_symlink_is_rejected() {
    use std::os::unix::fs::symlink;

    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let denied_dir = home.path().join(".ssh");
    std::fs::create_dir_all(&denied_dir).unwrap();
    let target = denied_dir.join("authorized_keys");
    let link = cwd.path().join("link");
    symlink(&target, &link).unwrap();

    let context = temp_env::with_var("HOME", Some(home.path()), || {
        ToolContext::with_temp_dirs(
            cwd.path().to_path_buf(),
            vec![],
            vec![],
            vec![],
            vec![home.path().to_path_buf()],
        )
    });

    let error = resolve_path_for_write_scheduling(&context, link.to_str().unwrap()).unwrap_err();
    assert!(error.contains("dangling symlink"), "{error}");
    assert!(
        !target.exists(),
        "a rejected dangling-link write must create nothing"
    );
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

#[cfg(target_os = "linux")]
#[test]
fn linux_os_only_paths_are_rejected_by_the_validator() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let context = temp_env::with_var("HOME", Some(home.path()), || {
        ToolContext::with_temp_dirs(cwd.path().to_path_buf(), vec![], vec![], vec![], vec![])
    });

    for path in ["/proc/self/environ", "/dev/fd/0", "/sys/kernel"] {
        let result = validate_path(&context, path);
        assert!(
            result.is_err(),
            "{path} must stay outside the in-process grants"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_dev_null_still_reaches_the_non_regular_file_rejection() {
    let args = serde_json::json!({ "path": "/dev/null" }).to_string();
    let error = execute_read(&ToolContext::from_current_process(), &args).unwrap_err();
    assert!(error.contains("is not a regular file"), "{error}");
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
    let home = root.path().join("home");
    let mut context = temp_env::with_var("HOME", Some(&home), || {
        ToolContext::with_temp_dirs(workspace, vec![], vec![], vec![], grants)
    });
    let nested = target.join("new/first.txt");
    let parent = target.parent().unwrap().join("parent.txt");
    let sibling = target.with_file_name("sibling");
    assert!(resolve_path_for_write_scheduling(&context, nested.to_str().unwrap()).is_ok());
    assert!(resolve_path_for_write_scheduling(&context, parent.to_str().unwrap()).is_err());
    assert!(resolve_path_for_write_scheduling(&context, sibling.to_str().unwrap()).is_err());

    let config = SandboxConfig::build(&context);
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

    temp_env::with_var("HOME", Some(&home), || {
        context.set_sandbox_policy(SandboxPolicy::ReadOnly);
    });
    let config = SandboxConfig::build(&context);
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
