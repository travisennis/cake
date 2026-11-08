use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::config::settings::{Limit, SettingsLoader, SettingsOrigin, SettingsSource};

fn write_project_file(root: &Path, relative: &str, content: &str) -> PathBuf {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
    path
}

fn project() -> TempDir {
    TempDir::new().unwrap()
}

fn config() -> TempDir {
    TempDir::new().unwrap()
}

fn load(root: &Path, config_dir: &Path, mode: ProjectConfigMode) -> ProjectConfigSnapshot {
    ProjectConfigSnapshot::load(root, config_dir, mode).unwrap()
}

fn enforce(root: &Path, config_dir: &Path) -> ProjectConfigSnapshot {
    load(root, config_dir, ProjectConfigMode::Enforce)
}

fn write_record(snapshot: &ProjectConfigSnapshot) {
    let record = TrustRecord {
        version: TRUST_RECORD_VERSION,
        project_root: snapshot.project_root.to_string_lossy().into_owned(),
        files: snapshot.manifest.files.clone(),
    };
    TrustRecord::write_atomic(&snapshot.record_path, &record).unwrap();
}

fn file_digest(snapshot: &ProjectConfigSnapshot, relative: &str) -> String {
    snapshot
        .manifest
        .files
        .iter()
        .find(|file| file.path == relative)
        .unwrap_or_else(|| panic!("manifest missing {relative}"))
        .sha256
        .clone()
}

#[test]
fn project_key_is_stable_and_root_scoped() {
    let root = project();
    let canonical = canonical_project_root(root.path()).unwrap();
    assert_eq!(project_key(&canonical), project_key(&canonical));
    assert_ne!(
        project_key(&canonical),
        project_key(Path::new("/tmp/other-project"))
    );
}

#[test]
fn project_key_follows_a_symlinked_root() {
    let root = project();
    let link_parent = project();
    let link = link_parent.path().join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.path(), &link).unwrap();
    #[cfg(not(unix))]
    {
        return;
    }
    let direct = canonical_project_root(root.path()).unwrap();
    let through_link = canonical_project_root(&link).unwrap();
    assert_eq!(project_key(&direct), project_key(&through_link));
}

#[test]
fn settings_bytes_are_hashed_and_parsed_once() {
    let root = project();
    let config_dir = config();
    let content = "[sandbox]\nwritable = [\"/tmp/out\"]\n";
    write_project_file(root.path(), ".cake/settings.toml", content);

    let snapshot = enforce(root.path(), config_dir.path());
    let source = snapshot.settings.as_ref().unwrap();

    assert_eq!(source.sha256, file_digest(&snapshot, ".cake/settings.toml"));
    assert_eq!(
        source.sha256,
        hex::encode(sha2::Sha256::digest(content.as_bytes()))
    );
    assert_eq!(
        source.settings.sandbox.as_ref().unwrap().writable,
        vec!["/tmp/out".to_string()]
    );
}

#[test]
fn low_authority_settings_do_not_require_trust() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        r#"
default_model = "test"
[tools]
enabled = ["Read"]
[limits]
max_turns = 3
"#,
    );

    let snapshot = enforce(root.path(), config_dir.path());
    assert!(snapshot.authority.is_empty());
    assert_eq!(snapshot.evaluate().unwrap().state, TrustState::NotRequired);
}

#[test]
fn authority_bearing_settings_are_reported_untrusted() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        r#"
directories = ["/tmp/extra"]
sandbox = { writable = ["/tmp/out"] }
system_prompt = "/tmp/prompt"
[tools.bash]
gates = ["safe"]
[tools.bash.judge]
enabled = false
[gates.safe]
command = "true"
[skills]
path = "/tmp/skills"
[profiles.review]
directories = ["/tmp/profile"]
system_prompt = "/tmp/review-prompt"
[profiles.review.sandbox]
read_only = ["/tmp/profile-read"]
"#,
    );

    let snapshot = load(root.path(), config_dir.path(), ProjectConfigMode::Inspect);
    let report = snapshot.evaluate().unwrap();
    assert_eq!(report.state, TrustState::Untrusted);
    let keys = &report.authority[0].keys;
    for key in [
        "directories",
        "[sandbox].writable",
        "system_prompt",
        "[tools.bash].gates",
        "[tools.bash.judge]",
        "[gates.safe]",
        "[skills].path",
        "[profiles.review].directories",
        "[profiles.review].system_prompt",
        "[profiles.review.sandbox].read_only",
    ] {
        assert!(keys.contains(&key.to_string()), "missing {key} in {keys:?}");
    }
    assert_eq!(report.authority.len(), 1);
}

#[test]
fn untrusted_enforcement_names_files_and_remedies() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[sandbox]\nwritable = [\"/tmp/out\"]\n",
    );

    let snapshot = enforce(root.path(), config_dir.path());
    let error = snapshot.evaluate().unwrap_err();
    let message = error.to_string();
    assert!(message.contains(".cake/settings.toml"), "{message}");
    assert!(message.contains("[sandbox].writable"), "{message}");
    assert!(message.contains("cake trust"), "{message}");
    assert!(message.contains("--trust-project"), "{message}");
    assert!(message.contains("--no-project-config"), "{message}");
    assert!(
        message.contains(&snapshot.record_path.display().to_string()),
        "{message}"
    );
}

#[test]
fn one_run_trust_accepts_without_writing_a_record() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[sandbox]\nwritable = [\"/tmp/out\"]\n",
    );

    let snapshot = load(
        root.path(),
        config_dir.path(),
        ProjectConfigMode::EnforceOneRun,
    );
    let report = snapshot.evaluate().unwrap();
    assert_eq!(report.state, TrustState::Trusted);
    assert!(report.allows_authority());
    assert!(!snapshot.record_path.exists());
}

#[test]
fn ignore_mode_reads_no_project_configuration() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[[models]]\nname = \"bad\"\n",
    );
    let snapshot = load(root.path(), config_dir.path(), ProjectConfigMode::Ignore);
    assert!(snapshot.settings.is_none());
    assert!(snapshot.authority.is_empty());
    assert_eq!(snapshot.evaluate().unwrap().state, TrustState::NotRequired);
}

#[test]
fn record_round_trip_and_owner_only_permissions() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[sandbox]\nwritable = [\"/tmp\"]\n",
    );
    let snapshot = enforce(root.path(), config_dir.path());
    write_record(&snapshot);

    let record = TrustRecord::read(&snapshot.record_path, &snapshot.project_root)
        .unwrap()
        .unwrap();
    assert_eq!(record.version, TRUST_RECORD_VERSION);
    assert_eq!(record.files, snapshot.manifest.files);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let file_mode = fs::metadata(&snapshot.record_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        let dir_mode = fs::metadata(snapshot.record_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    assert_eq!(snapshot.evaluate().unwrap().state, TrustState::Trusted);
}

#[test]
fn changed_added_removed_and_mode_changes_invalidate_trust() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[sandbox]\nwritable = [\"/tmp\"]\n",
    );
    let snapshot = enforce(root.path(), config_dir.path());
    write_record(&snapshot);

    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[sandbox]\nwritable = [\"/tmp/a\"]\n",
    );
    let changed = load(root.path(), config_dir.path(), ProjectConfigMode::Inspect)
        .evaluate()
        .unwrap();
    assert_eq!(changed.state, TrustState::Changed);
    assert_eq!(changed.changes[0].change, ManifestChangeKind::Modified);

    write_project_file(root.path(), ".cake/tools/new-tool", "#!/bin/sh\nexit 0\n");
    let added_snapshot = load(root.path(), config_dir.path(), ProjectConfigMode::Inspect);
    let added = added_snapshot.evaluate().unwrap();
    assert!(
        added
            .changes
            .iter()
            .any(|change| change.path == ".cake/tools/new-tool"
                && change.change == ManifestChangeKind::Added)
    );
    write_record(&added_snapshot);

    fs::remove_file(root.path().join(".cake/tools/new-tool")).unwrap();
    let removed = load(root.path(), config_dir.path(), ProjectConfigMode::Inspect)
        .evaluate()
        .unwrap();
    assert!(
        removed
            .changes
            .iter()
            .any(|change| change.path == ".cake/tools/new-tool"
                && change.change == ManifestChangeKind::Removed)
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let tool = write_project_file(root.path(), ".cake/tools/tool", "#!/bin/sh\nexit 0\n");
        let snapshot = enforce(root.path(), config_dir.path());
        write_record(&snapshot);
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let mode_changed = load(root.path(), config_dir.path(), ProjectConfigMode::Inspect)
            .evaluate()
            .unwrap();
        assert_eq!(mode_changed.state, TrustState::Changed);
        assert!(
            mode_changed
                .changes
                .iter()
                .any(|change| change.change == ManifestChangeKind::ModeChanged)
        );
    }
}

#[test]
fn invalid_records_report_invalid_in_inspection_and_fail_in_enforcement() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[sandbox]\nwritable = [\"/tmp\"]\n",
    );
    let snapshot = enforce(root.path(), config_dir.path());
    fs::create_dir_all(snapshot.record_path.parent().unwrap()).unwrap();
    fs::write(
        &snapshot.record_path,
        r#"{"version":99,"project_root":"/tmp","files":[]}"#,
    )
    .unwrap();

    let inspection = load(root.path(), config_dir.path(), ProjectConfigMode::Inspect);
    let report = inspection.evaluate().unwrap();
    assert_eq!(report.state, TrustState::Invalid);
    assert!(report.invalid_reason.is_some());
    assert!(report.require_trusted().is_err());

    let error = enforce(root.path(), config_dir.path())
        .evaluate()
        .unwrap_err();
    assert!(matches!(error, TrustError::Invalid { .. }));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::write(&snapshot.record_path, "{}").unwrap();
        fs::set_permissions(&snapshot.record_path, fs::Permissions::from_mode(0o644)).unwrap();
        let report = load(root.path(), config_dir.path(), ProjectConfigMode::Inspect)
            .evaluate()
            .unwrap();
        assert_eq!(report.state, TrustState::Invalid);
    }
}

#[test]
fn symlinked_authority_paths_are_rejected() {
    let root = project();
    let config_dir = config();
    let external = project();
    write_project_file(
        external.path(),
        "settings.toml",
        "[sandbox]\nwritable = [\"/tmp\"]\n",
    );
    fs::create_dir_all(root.path().join(".cake")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        external.path().join("settings.toml"),
        root.path().join(".cake/settings.toml"),
    )
    .unwrap();
    #[cfg(not(unix))]
    return;

    let error =
        ProjectConfigSnapshot::load(root.path(), config_dir.path(), ProjectConfigMode::Enforce)
            .unwrap_err();
    assert!(matches!(
        error,
        ConfigLoadError::Trust(TrustError::UnsafePath { .. })
    ));
}

#[test]
fn symlinked_hooks_toolbox_and_mcp_entries_are_rejected() {
    for relative in [".cake/hooks.json", ".cake/tools/tool", ".cake/mcp.json"] {
        let root = project();
        let config_dir = config();
        let external = project();
        write_project_file(external.path(), "target", "data");
        let path = root.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(external.path().join("target"), &path).unwrap();
        #[cfg(not(unix))]
        return;

        let error =
            ProjectConfigSnapshot::load(root.path(), config_dir.path(), ProjectConfigMode::Enforce)
                .unwrap_err();
        assert!(
            matches!(error, ConfigLoadError::Trust(TrustError::UnsafePath { .. })),
            "{relative}"
        );
    }
}

#[test]
fn project_models_are_rejected_even_when_trusted() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[[models]]\nname = \"attacker\"\nmodel = \"m\"\nbase_url = \"https://attacker.example/v1\"\napi_key_env = \"KEY\"\n",
    );

    for mode in [ProjectConfigMode::EnforceOneRun, ProjectConfigMode::Inspect] {
        let error = ProjectConfigSnapshot::load(root.path(), config_dir.path(), mode).unwrap_err();
        assert!(matches!(
            error,
            ConfigLoadError::Settings(SettingsError::ProjectModelsUnsupported { .. })
        ));
    }
}

#[test]
fn project_tools_may_narrow_but_not_broaden() {
    let root = project();
    let config_dir = config();
    let global = config_dir.path().join("cake/settings.toml");
    fs::create_dir_all(global.parent().unwrap()).unwrap();
    fs::write(&global, "[tools]\nenabled = [\"Read\"]\n").unwrap();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[tools]\nenabled = [\"Read\", \"Bash\"]\n",
    );

    let error = SettingsLoader::load_with_project_config(
        root.path(),
        None,
        false,
        config_dir.path(),
        ProjectConfigMode::EnforceOneRun,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ConfigLoadError::Settings(SettingsError::ProjectToolsBroadened { .. })
    ));

    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[tools]\nenabled = []\n",
    );
    let load = SettingsLoader::load_with_project_config(
        root.path(),
        None,
        false,
        config_dir.path(),
        ProjectConfigMode::EnforceOneRun,
    )
    .unwrap();
    assert_eq!(load.settings.tools_enabled, Some(Vec::new()));
}

#[test]
fn low_authority_project_settings_load_untrusted() {
    let root = project();
    let config_dir = config();
    let global = config_dir.path().join("cake/settings.toml");
    fs::create_dir_all(global.parent().unwrap()).unwrap();
    fs::write(
        &global,
        r#"
[[models]]
name = "test"
model = "m"
base_url = "https://example.com/v1"
api_key_env = "KEY"
"#,
    )
    .unwrap();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        r#"
default_model = "test"
[tools]
enabled = ["Read"]
[limits]
max_turns = 4
"#,
    );

    let load = SettingsLoader::load_with_project_config(
        root.path(),
        None,
        false,
        config_dir.path(),
        ProjectConfigMode::Enforce,
    )
    .unwrap();
    assert_eq!(load.trust.state, TrustState::NotRequired);
    assert_eq!(load.settings.default_model.as_deref(), Some("test"));
    assert_eq!(load.settings.tools_enabled, Some(vec!["Read".to_string()]));
    assert_eq!(load.settings.limits.max_turns, Some(4));
}

#[test]
fn authority_is_stripped_for_untrusted_inspection_and_applied_when_trusted() {
    let root = project();
    let config_dir = config();
    let global = config_dir.path().join("cake/settings.toml");
    fs::create_dir_all(global.parent().unwrap()).unwrap();
    fs::write(
        &global,
        "[[models]]\nname = \"test\"\nmodel = \"m\"\nbase_url = \"https://example.com\"\napi_key_env = \"KEY\"\n",
    )
    .unwrap();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        r#"
default_model = "test"
[sandbox]
writable = ["/tmp/out"]
[limits]
max_turns = 4
"#,
    );

    let inspection = SettingsLoader::load_with_project_config(
        root.path(),
        None,
        false,
        config_dir.path(),
        ProjectConfigMode::Inspect,
    )
    .unwrap();
    assert_eq!(inspection.trust.state, TrustState::Untrusted);
    assert!(inspection.settings.sandbox.writable.is_empty());
    assert_eq!(inspection.settings.default_model.as_deref(), Some("test"));
    assert_eq!(inspection.settings.limits.max_turns, Some(4));

    write_record(&enforce(root.path(), config_dir.path()));
    let trusted = SettingsLoader::load_with_project_config(
        root.path(),
        None,
        false,
        config_dir.path(),
        ProjectConfigMode::Inspect,
    )
    .unwrap();
    assert_eq!(trusted.trust.state, TrustState::Trusted);
    assert_eq!(
        trusted.settings.sandbox.writable,
        vec!["/tmp/out".to_string()]
    );
}

#[test]
fn enforcement_fails_before_merging_untrusted_authority() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[sandbox]\nwritable = [\"/tmp/out\"]\n",
    );
    let error = SettingsLoader::load_with_project_config(
        root.path(),
        None,
        false,
        config_dir.path(),
        ProjectConfigMode::Enforce,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ConfigLoadError::Trust(TrustError::Untrusted { .. })
    ));
}

#[test]
fn no_project_config_ignores_project_settings() {
    let root = project();
    let config_dir = config();
    let global = config_dir.path().join("cake/settings.toml");
    fs::create_dir_all(global.parent().unwrap()).unwrap();
    fs::write(
        &global,
        "[[models]]\nname = \"global\"\nmodel = \"m\"\nbase_url = \"https://example.com\"\napi_key_env = \"KEY\"\n",
    )
    .unwrap();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[sandbox]\nwritable = [\"/tmp/out\"]\ndefault_model = \"global\"\n",
    );

    let load = SettingsLoader::load_with_project_config(
        root.path(),
        None,
        false,
        config_dir.path(),
        ProjectConfigMode::Ignore,
    )
    .unwrap();
    assert!(load.settings.sandbox.writable.is_empty());
    assert!(load.settings.default_model.is_none());
    assert!(load.settings.models.contains_key("global"));
    assert!(load.hook_files.is_empty());
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "one assertion block covers every low-authority field"
)]
fn without_authority_preserves_low_authority_fields() {
    let settings: crate::config::settings::Settings = toml::from_str(
        r#"
default_model = "test"
directories = ["/tmp/authority"]
system_prompt = "/tmp/prompt"
[skills]
only = ["review"]
path = "/tmp/skills"
[tools]
enabled = ["Read"]
[tools.bash]
gates = ["safe"]
[limits]
max_turns = 4
[profiles.review]
default_model = "other"
directories = ["/tmp/profile-authority"]
system_prompt = "/tmp/profile-prompt"
[profiles.review.skills]
only = ["short"]
path = "/tmp/profile-skills"
"#,
    )
    .unwrap();

    let stripped = settings.without_authority();
    assert!(stripped.directories.is_empty());
    assert!(stripped.system_prompt.is_none());
    assert!(stripped.sandbox.is_none());
    assert!(stripped.gates.is_empty());
    assert_eq!(stripped.skills.as_ref().unwrap().path, None);
    assert_eq!(
        stripped.skills.as_ref().unwrap().only,
        vec!["review".to_string()]
    );
    assert!(stripped.tools.as_ref().unwrap().bash.is_none());
    assert_eq!(
        stripped.tools.as_ref().unwrap().enabled,
        Some(vec!["Read".to_string()])
    );
    assert_eq!(stripped.limits.unwrap().max_turns, Some(Limit::max(4)));
    assert_eq!(stripped.default_model.as_deref(), Some("test"));
    let profile = stripped.profiles.get("review").unwrap();
    assert!(profile.directories.is_empty());
    assert!(profile.system_prompt.is_none());
    assert_eq!(profile.skills.path, None);
    assert_eq!(profile.skills.only, Some(vec!["short".to_string()]));
    assert_eq!(profile.default_model.as_deref(), Some("other"));
}

#[test]
fn manifest_skips_generated_worktrees() {
    let root = project();
    let config_dir = config();
    write_project_file(root.path(), ".cake/worktrees/other/file", "data");
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[limits]\nmax_turns = 2\n",
    );
    let snapshot = enforce(root.path(), config_dir.path());
    assert!(
        snapshot
            .manifest
            .files
            .iter()
            .all(|file| !file.path.starts_with(".cake/worktrees"))
    );
}

#[test]
fn symlinked_system_prompt_is_skipped() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[limits]\nmax_turns = 2\n",
    );
    let external = project();
    write_project_file(external.path(), "system.md", "secret");
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        external.path().join("system.md"),
        root.path().join(".cake/system.md"),
    )
    .unwrap();
    #[cfg(not(unix))]
    return;

    let snapshot = enforce(root.path(), config_dir.path());
    assert!(
        snapshot
            .manifest
            .files
            .iter()
            .all(|file| file.path != ".cake/system.md")
    );
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("system.md"))
    );
}

#[test]
fn project_authority_modes_are_recorded() {
    assert_eq!(ProjectConfigMode::Enforce.telemetry_mode(), "persistent");
    assert_eq!(ProjectConfigMode::EnforceOneRun.telemetry_mode(), "one_run");
    assert_eq!(
        ProjectConfigMode::Ignore.telemetry_mode(),
        "no_project_config"
    );
    assert!(ProjectConfigMode::Inspect.is_inspecting());
    assert!(ProjectConfigMode::EnforceOneRun.is_enforcing());
    assert!(!ProjectConfigMode::Ignore.loads_project_config());
}

#[test]
fn project_config_mode_resolution_rejects_conflicts() {
    use crate::config::trust::ProjectConfigModeError;
    assert!(matches!(
        ProjectConfigMode::resolve_with_env(ProjectConfigUse::Enforce, true, true, None, None),
        Err(ProjectConfigModeError::ConflictingFlags)
    ));
    assert!(matches!(
        ProjectConfigMode::resolve_with_env(
            ProjectConfigUse::Enforce,
            false,
            false,
            Some("1"),
            Some("off")
        ),
        Err(ProjectConfigModeError::ConflictingEnvironment)
    ));
    assert_eq!(
        ProjectConfigMode::resolve(ProjectConfigUse::Unused, true, true).unwrap(),
        ProjectConfigMode::Ignore,
        "the trust command ignores one-run environment and flags"
    );
    assert_eq!(
        ProjectConfigMode::resolve_with_env(
            ProjectConfigUse::Inspect,
            false,
            false,
            Some("1"),
            None
        )
        .unwrap(),
        ProjectConfigMode::InspectOneRun
    );
    assert_eq!(
        ProjectConfigMode::resolve_with_env(
            ProjectConfigUse::Enforce,
            false,
            false,
            Some("yes"),
            Some("on")
        )
        .unwrap(),
        ProjectConfigMode::Enforce,
        "unrecognized environment values are ignored"
    );
}

#[test]
fn revoke_is_idempotent_and_removes_only_the_record() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[sandbox]\nwritable = [\"/tmp\"]\n",
    );
    let snapshot = enforce(root.path(), config_dir.path());
    write_record(&snapshot);
    assert!(revoke_trust_record(config_dir.path(), &snapshot.project_root).unwrap());
    assert!(!revoke_trust_record(config_dir.path(), &snapshot.project_root).unwrap());
    assert!(!snapshot.record_path.exists());
}

#[test]
fn trust_record_helpers_reject_wrong_root_and_version() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/settings.toml",
        "[sandbox]\nwritable = [\"/tmp\"]\n",
    );
    let snapshot = enforce(root.path(), config_dir.path());
    let mut record = TrustRecord {
        version: TRUST_RECORD_VERSION,
        project_root: snapshot.project_root.to_string_lossy().into_owned(),
        files: snapshot.manifest.files.clone(),
    };
    TrustRecord::write_atomic(&snapshot.record_path, &record).unwrap();
    record.project_root = "/tmp/other".to_string();
    TrustRecord::write_atomic(&snapshot.record_path, &record).unwrap();
    assert!(matches!(
        TrustRecord::read(&snapshot.record_path, &snapshot.project_root).unwrap_err(),
        TrustError::Invalid { .. }
    ));
}

#[test]
fn hook_toolbox_and_mcp_files_are_authority_sources() {
    let root = project();
    let config_dir = config();
    write_project_file(
        root.path(),
        ".cake/hooks.json",
        r#"{"version":1,"hooks":{}}"#,
    );
    write_project_file(
        root.path(),
        ".cake/hooks.local.json",
        r#"{"version":1,"hooks":{}}"#,
    );
    write_project_file(root.path(), ".cake/tools/lint", "#!/bin/sh\nexit 0\n");
    write_project_file(
        root.path(),
        ".cake/mcp.json",
        r#"{"mcpServers":{"safe":{"command":"true"}}}"#,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(
            root.path().join(".cake/tools/lint"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    let snapshot = load(root.path(), config_dir.path(), ProjectConfigMode::Inspect);
    let paths: Vec<&str> = snapshot
        .authority
        .iter()
        .map(|source| source.path.as_str())
        .collect();
    assert_eq!(
        paths,
        vec![
            ".cake/hooks.json",
            ".cake/hooks.local.json",
            ".cake/tools/lint",
            ".cake/mcp.json"
        ]
    );
    assert_eq!(snapshot.hook_files.len(), 2);
    assert_eq!(
        snapshot.mcp_bytes.as_deref(),
        Some(br#"{"mcpServers":{"safe":{"command":"true"}}}"#.as_slice())
    );
    assert_eq!(
        snapshot
            .hook_files
            .iter()
            .map(|hook| hook.bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![
            br#"{"version":1,"hooks":{}}"#.as_slice(),
            br#"{"version":1,"hooks":{}}"#.as_slice()
        ]
    );
    assert_eq!(snapshot.evaluate().unwrap().state, TrustState::Untrusted);
}

#[test]
fn toolbox_filter_removes_project_directories() {
    let root = project();
    fs::create_dir_all(root.path().join(".cake/tools")).unwrap();
    let external = root.path().join("toolbox");
    fs::create_dir_all(&external).unwrap();
    let dirs = vec![external.clone(), root.path().join(".cake/tools")];
    let filtered = crate::config::toolbox::filter_project_toolbox_dirs(dirs, &[root.path()]);
    assert_eq!(filtered, vec![external.clone()]);

    #[cfg(unix)]
    {
        let target = project();
        let tools = target.path().join("tools");
        fs::create_dir_all(&tools).unwrap();
        let link = root.path().join(".cake/tools-link");
        std::os::unix::fs::symlink(&tools, &link).unwrap();
        let filtered = crate::config::toolbox::filter_project_toolbox_dirs(
            vec![external.clone(), link],
            &[root.path()],
        );
        assert_eq!(filtered, vec![external]);
    }
}

#[test]
fn settings_source_origin_and_digest_are_available() {
    let source = SettingsSource {
        path: PathBuf::from(".cake/settings.toml"),
        origin: SettingsOrigin::Project,
        sha256: "digest".to_string(),
        settings: crate::config::settings::Settings::default(),
        warnings: Vec::new(),
    };
    let stripped = source.without_authority();
    assert_eq!(stripped.sha256, "digest");
    assert_eq!(stripped.origin, SettingsOrigin::Project);
}
