//! Pure check builders for `cake doctor`.
//!
//! The builders consume collected facts and never read the filesystem,
//! environment, or clock. Every non-ok check carries a structured fix hint.

use crate::cli::debug::project_trust_warning_message;
use crate::cli::{
    CHECK_CONFIG_CREDENTIAL, CHECK_CONFIG_MODEL, CHECK_CONFIG_MODEL_CONFIGURED,
    CHECK_CONFIG_MODEL_MISSING, CHECK_CONFIG_PROJECT_TRUST, CHECK_GRANTS_NONE, CHECK_GRANTS_PATH,
    CHECK_GRANTS_PATH_OK, CHECK_GRANTS_POLICY, CHECK_SETTINGS_GLOBAL, CHECK_SETTINGS_LOAD_FAILED,
    CHECK_SETTINGS_PROFILE, CHECK_SETTINGS_PROJECT, CHECK_SETTINGS_UNKNOWN_KEY, DiagnosticFix,
};
use crate::clients::GrantReport;
use crate::config::trust::{ProjectTrustReport, TrustState};

use crate::cli::doctor::report::{DoctorCheck, DoctorGroup, DoctorReport, DoctorSummary, GroupId};
use crate::cli::doctor::{CredentialFact, DoctorFacts, DoctorFailureFacts, SettingsSourceReport};

const CONFIGURED_GRANT_SOURCES: [(&str, &str); 3] = [
    ("directories", "Directory grant"),
    ("sandbox_writable", "Writable grant"),
    ("sandbox_read_only", "Read-only grant"),
];

/// Build the complete success report from collected facts.
pub(super) fn build_success(facts: &DoctorFacts<'_>) -> DoctorReport {
    let mut summary = DoctorSummary::new(facts.workspace.display().to_string());
    summary.settings_source_count = facts.sources.len();
    summary.profile = facts.profile.map(str::to_string);
    summary.model.clone_from(&facts.model.name);
    summary.model_source = facts.model.source;
    summary.model_configured = facts.model.configured;
    summary.credential_present = facts.model.credential.as_ref().map(|fact| fact.present);
    summary.policy = Some(facts.grants.policy.label());
    summary.policy_source = Some(facts.grants.policy_source.label());
    summary.sandbox_enforced = Some(facts.grants.policy.applies_os_sandbox());
    summary.configured_writable_count = Some(configured_entry_count(
        facts.grants,
        &["directories", "sandbox_writable"],
    ));
    summary.configured_read_only_count =
        Some(configured_entry_count(facts.grants, &["sandbox_read_only"]));

    let groups = vec![
        settings_group(facts),
        model_group(facts),
        sandbox_group(facts),
        project_group(facts),
    ];
    DoctorReport::from_groups(summary, groups, Vec::new())
}

/// Build the partial report for a settings load failure.
pub(super) fn build_failure(facts: &DoctorFailureFacts<'_>) -> DoctorReport {
    let mut summary = DoctorSummary::new(facts.workspace.display().to_string());
    summary.settings_source_count = facts.sources.len();
    summary.profile = facts.profile.map(str::to_string);

    let mut group = DoctorGroup::new(GroupId::Settings);
    group.push(DoctorCheck::error(
        CHECK_SETTINGS_LOAD_FAILED,
        "Settings load",
        facts.message.clone(),
        summary_fix("correct the settings file named in the error, then rerun cake doctor"),
    ));

    DoctorReport::from_groups(
        summary,
        vec![group],
        vec![GroupId::Model, GroupId::Sandbox, GroupId::Project],
    )
}

fn settings_group(facts: &DoctorFacts<'_>) -> DoctorGroup {
    let mut group = DoctorGroup::new(GroupId::Settings);
    for source in facts.sources {
        if source.origin == "global" {
            group.push(DoctorCheck::ok(
                CHECK_SETTINGS_GLOBAL,
                "Global settings",
                format!("{} ({})", source.path, presence(source.present)),
            ));
        } else {
            group.push(DoctorCheck::ok(
                CHECK_SETTINGS_PROJECT,
                "Project settings",
                project_settings_message(source, facts),
            ));
        }
    }
    if !facts.project_config_mode.loads_project_config() {
        let path = facts
            .workspace
            .join(".cake")
            .join("settings.toml")
            .display()
            .to_string();
        group.push(DoctorCheck::ok(
            CHECK_SETTINGS_PROJECT,
            "Project settings",
            format!("{path} (not read; project config disabled)"),
        ));
    }
    group.push(DoctorCheck::ok(
        CHECK_SETTINGS_PROFILE,
        "Profile",
        facts.profile.unwrap_or("(none)"),
    ));
    for warning in &facts.load.settings.warnings {
        group.push(DoctorCheck::warning(
            CHECK_SETTINGS_UNKNOWN_KEY,
            "Settings key",
            warning.clone(),
            summary_fix("remove or correct the key named in the warning"),
        ));
    }
    group
}

fn project_settings_message(source: &SettingsSourceReport, facts: &DoctorFacts<'_>) -> String {
    if !source.present {
        return format!("{} (not present)", source.path);
    }
    let trust = &facts.load.trust;
    if trust.authority_file_count() == 0 {
        format!("{} (present)", source.path)
    } else if trust.allows_authority() {
        format!("{} (present; authority applied)", source.path)
    } else {
        format!("{} (present; authority not applied)", source.path)
    }
}

fn model_group(facts: &DoctorFacts<'_>) -> DoctorGroup {
    let mut group = DoctorGroup::new(GroupId::Model);
    match (facts.model.name.as_deref(), facts.model.source) {
        (None, _) => group.push(DoctorCheck::warning(
            CHECK_CONFIG_MODEL_MISSING,
            "Model",
            "no model selected; set default_model or pass --model",
            summary_fix("set default_model or pass --model <name>"),
        )),
        (Some(name), Some(source)) if facts.model.configured => {
            group.push(DoctorCheck::ok(
                CHECK_CONFIG_MODEL_CONFIGURED,
                "Model",
                format!("{name} (from {source})"),
            ));
            if let Some(credential) = &facts.model.credential {
                group.push(credential_check(credential));
            }
        },
        (Some(name), _) => group.push(DoctorCheck::warning(
            CHECK_CONFIG_MODEL,
            "Model",
            format!("selected model '{name}' is not defined in settings.toml"),
            summary_fix("pass a configured model name or add a model definition entry"),
        )),
    }
    group
}

fn credential_check(credential: &CredentialFact) -> DoctorCheck {
    if credential.present {
        DoctorCheck::ok(
            CHECK_CONFIG_CREDENTIAL,
            "Credential",
            format!("{} (set; not verified)", credential.name),
        )
    } else {
        DoctorCheck::warning(
            CHECK_CONFIG_CREDENTIAL,
            "Credential",
            format!("{} (not set)", credential.name),
            summary_fix(format!("set {} in the environment", credential.name)),
        )
    }
}

fn sandbox_group(facts: &DoctorFacts<'_>) -> DoctorGroup {
    let mut group = DoctorGroup::new(GroupId::Sandbox);
    group.push(DoctorCheck::ok(
        CHECK_GRANTS_POLICY,
        "Policy",
        policy_message(facts),
    ));
    let mut any_configured = false;
    for (id, label) in CONFIGURED_GRANT_SOURCES {
        for source in facts.grants.sources.iter().filter(|source| source.id == id) {
            any_configured |= !source.entries.is_empty() || !source.notices.is_empty();
            for notice in &source.notices {
                group.push(DoctorCheck::warning(
                    CHECK_GRANTS_PATH,
                    label,
                    notice.clone(),
                    summary_fix(
                        "remove the empty path or correct the setting named in the warning",
                    ),
                ));
            }
            for entry in &source.entries {
                group.push(grant_entry_check(
                    label,
                    &entry.path.display().to_string(),
                    entry.status.label(),
                    entry.detail.as_deref(),
                ));
            }
        }
    }
    if !any_configured {
        group.push(DoctorCheck::ok(
            CHECK_GRANTS_NONE,
            "Grants",
            "no configured directory, writable, or read-only grant settings",
        ));
    }
    group
}

fn policy_message(facts: &DoctorFacts<'_>) -> String {
    let policy = facts.grants.policy;
    let enforcement = if policy.applies_os_sandbox() {
        "OS sandbox applied"
    } else {
        "OS sandbox not applied (danger-full-access)"
    };
    format!(
        "{} (from {}; {enforcement})",
        policy.label(),
        facts.grants.policy_source.label()
    )
}

fn grant_entry_check(
    label: &'static str,
    path: &str,
    status: &str,
    detail: Option<&str>,
) -> DoctorCheck {
    if status == "granted" {
        return DoctorCheck::ok(CHECK_GRANTS_PATH_OK, label, format!("{path} (granted)"));
    }
    let message = detail.map_or_else(|| format!("{path} ({status})"), str::to_string);
    DoctorCheck::warning(
        CHECK_GRANTS_PATH,
        label,
        message,
        summary_fix(grant_fix(status)),
    )
}

fn grant_fix(status: &str) -> &'static str {
    match status {
        "would-create" => "create the directory ahead of time, or let a writable run create it",
        "missing" => "create the path or remove the grant",
        "not-a-directory" => "point the setting at a directory or remove the grant",
        "excluded" => "make the path inspectable or remove the grant",
        _ => "remove the grant",
    }
}

fn project_group(facts: &DoctorFacts<'_>) -> DoctorGroup {
    let mut group = DoctorGroup::new(GroupId::Project);
    let trust = &facts.load.trust;
    match project_trust_warning_message(trust) {
        Some(message) => group.push(DoctorCheck::warning(
            CHECK_CONFIG_PROJECT_TRUST,
            "Project trust",
            message,
            DiagnosticFix {
                summary: "review the project configuration and run cake trust".to_string(),
                command: Some("cake trust"),
            },
        )),
        None => group.push(DoctorCheck::ok(
            CHECK_CONFIG_PROJECT_TRUST,
            "Project trust",
            trusted_message(trust),
        )),
    }
    group
}

fn trusted_message(trust: &ProjectTrustReport) -> String {
    if trust.state == TrustState::NotRequired {
        return "not_required".to_string();
    }
    match trust.authority_file_count() {
        0 => "trusted".to_string(),
        1 => "trusted (1 authority file)".to_string(),
        count => format!("trusted ({count} authority files)"),
    }
}

fn configured_entry_count(report: &GrantReport, ids: &[&str]) -> usize {
    report
        .sources
        .iter()
        .filter(|source| ids.contains(&source.id))
        .map(|source| source.entries.len())
        .sum()
}

const fn presence(present: bool) -> &'static str {
    if present { "present" } else { "not present" }
}

fn summary_fix(summary: impl Into<String>) -> DiagnosticFix {
    DiagnosticFix {
        summary: summary.into(),
        command: None,
    }
}
