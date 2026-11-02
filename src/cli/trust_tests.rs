use clap::Parser;

use crate::cli::TrustCommand;

#[test]
fn default_action_is_review() {
    let command = TrustCommand::try_parse_from(["trust"]).unwrap();
    assert!(!command.status);
    assert!(!command.revoke);
    assert!(!command.check);
    assert!(!command.json);
}

#[test]
fn actions_parse() {
    assert!(
        TrustCommand::try_parse_from(["trust", "--status"])
            .unwrap()
            .status
    );
    assert!(
        TrustCommand::try_parse_from(["trust", "--revoke"])
            .unwrap()
            .revoke
    );
    assert!(
        TrustCommand::try_parse_from(["trust", "--check"])
            .unwrap()
            .check
    );
    assert!(
        TrustCommand::try_parse_from(["trust", "--status", "--json"])
            .unwrap()
            .json
    );
}

#[test]
fn json_requires_status() {
    assert!(TrustCommand::try_parse_from(["trust", "--json"]).is_err());
    assert!(TrustCommand::try_parse_from(["trust", "--check", "--json"]).is_err());
}

#[test]
fn actions_conflict() {
    assert!(TrustCommand::try_parse_from(["trust", "--status", "--revoke"]).is_err());
    assert!(TrustCommand::try_parse_from(["trust", "--status", "--check"]).is_err());
    assert!(TrustCommand::try_parse_from(["trust", "--revoke", "--check"]).is_err());
}

use crate::config::trust::{
    AuthorityKind, AuthoritySource, ManifestChange, ManifestChangeKind, ProjectConfigMode,
    ProjectTrustReport, TrustState,
};

fn report(state: TrustState) -> ProjectTrustReport {
    ProjectTrustReport {
        state,
        mode: ProjectConfigMode::Inspect,
        project_root: std::path::PathBuf::from("/tmp/project"),
        project_key: "key".to_string(),
        record_path: std::path::PathBuf::from("/tmp/trust/key.json"),
        authority: Vec::new(),
        changes: Vec::new(),
        invalid_reason: None,
    }
}

#[test]
fn status_text_reports_not_required_without_remedies() {
    let text = super::render_status_text(&report(TrustState::NotRequired));
    assert!(text.contains("Project trust: not_required"));
    assert!(!text.contains("Run `cake trust`"));
}

#[test]
fn status_text_reports_untrusted_authority_and_remedies() {
    let mut report = report(TrustState::Untrusted);
    report.authority = vec![
        AuthoritySource {
            path: ".cake/settings.toml".to_string(),
            kind: AuthorityKind::Settings,
            keys: vec!["[sandbox].writable".to_string()],
            executable: false,
        },
        AuthoritySource {
            path: ".cake/tools/lint".to_string(),
            kind: AuthorityKind::ToolboxFile,
            keys: Vec::new(),
            executable: true,
        },
    ];
    let text = super::render_status_text(&report);
    assert!(text.contains("Project trust: untrusted"));
    assert!(text.contains(".cake/settings.toml: [sandbox].writable"));
    assert!(text.contains(".cake/tools/lint (executable)"));
    assert!(text.contains("--trust-project"));
    assert!(text.contains("--no-project-config"));
}

#[test]
fn status_text_reports_changes_and_invalid_reasons() {
    let mut changed = report(TrustState::Changed);
    changed.changes = vec![ManifestChange {
        path: ".cake/settings.toml".to_string(),
        change: ManifestChangeKind::Modified,
    }];
    let text = super::render_status_text(&changed);
    assert!(text.contains("modified: .cake/settings.toml"));
    assert!(text.contains("Run `cake trust`"));

    let mut invalid = report(TrustState::Invalid);
    invalid.invalid_reason = Some("unsupported version".to_string());
    let text = super::render_status_text(&invalid);
    assert!(text.contains("Reason: unsupported version"));
    assert!(!text.contains("Run `cake trust`"));
}
