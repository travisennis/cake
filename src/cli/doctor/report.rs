//! The grouped report model `cake doctor` projects to text and JSON.
//!
//! The model is pure: it holds no filesystem handles, environment reads, or
//! clock values. Builders in [`super::checks`] produce it from collected
//! facts, and renderers in [`super::render`] project it. Counts are derived
//! from the checks so a rendered total cannot drift from the rows.

use serde::Serialize;

use crate::cli::{DiagnosticCheck, DiagnosticFix, DiagnosticStatus};

/// One fixed section of the doctor report.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum GroupId {
    Settings,
    Model,
    Sandbox,
    Project,
}

impl GroupId {
    /// The stable machine-readable identifier used in findings.
    pub(super) const fn id(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Model => "model",
            Self::Sandbox => "sandbox",
            Self::Project => "project",
        }
    }

    /// The human-readable group title used in text output.
    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::Settings => "Settings",
            Self::Model => "Model",
            Self::Sandbox => "Sandbox",
            Self::Project => "Project",
        }
    }
}

/// One report group and its checks in render order.
#[derive(Serialize)]
pub(super) struct DoctorGroup {
    pub(super) id: GroupId,
    pub(super) title: &'static str,
    pub(super) checks: Vec<DoctorCheck>,
}

impl DoctorGroup {
    /// Start an empty group tagged with its fixed id and title.
    pub(super) const fn new(id: GroupId) -> Self {
        Self {
            id,
            title: id.title(),
            checks: Vec::new(),
        }
    }

    /// Append a check to this group.
    pub(super) fn push(&mut self, check: DoctorCheck) {
        self.checks.push(check);
    }
}

/// One check row, including the visible passing checks.
#[derive(Serialize)]
pub(super) struct DoctorCheck {
    pub(super) id: &'static str,
    pub(super) label: &'static str,
    pub(super) status: DiagnosticStatus,
    pub(super) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) fix: Option<DiagnosticFix>,
}

impl DoctorCheck {
    /// A passing check; passing checks never carry a fix.
    pub(super) fn ok(id: &'static str, label: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            label,
            status: DiagnosticStatus::Ok,
            message: message.into(),
            fix: None,
        }
    }

    /// A warning check; every warning must name a repair hint.
    pub(super) fn warning(
        id: &'static str,
        label: &'static str,
        message: impl Into<String>,
        fix: DiagnosticFix,
    ) -> Self {
        Self {
            id,
            label,
            status: DiagnosticStatus::Warning,
            message: message.into(),
            fix: Some(fix),
        }
    }

    /// An error check; every error must name a repair hint.
    pub(super) fn error(
        id: &'static str,
        label: &'static str,
        message: impl Into<String>,
        fix: DiagnosticFix,
    ) -> Self {
        Self {
            id,
            label,
            status: DiagnosticStatus::Error,
            message: message.into(),
            fix: Some(fix),
        }
    }
}

/// Summary scalars shared by text and JSON output.
#[derive(Serialize)]
pub(super) struct DoctorSummary {
    pub(super) workspace: String,
    pub(super) check_count: usize,
    pub(super) ok_count: usize,
    pub(super) warning_count: usize,
    pub(super) error_count: usize,
    pub(super) incomplete: bool,
    pub(super) settings_source_count: usize,
    pub(super) profile: Option<String>,
    pub(super) model: Option<String>,
    pub(super) model_source: Option<&'static str>,
    pub(super) model_configured: bool,
    pub(super) credential_present: Option<bool>,
    pub(super) policy: Option<&'static str>,
    pub(super) policy_source: Option<&'static str>,
    pub(super) sandbox_enforced: Option<bool>,
    pub(super) configured_writable_count: Option<usize>,
    pub(super) configured_read_only_count: Option<usize>,
}

impl DoctorSummary {
    /// Start a summary with zero counts and no context fields set.
    pub(super) const fn new(workspace: String) -> Self {
        Self {
            workspace,
            check_count: 0,
            ok_count: 0,
            warning_count: 0,
            error_count: 0,
            incomplete: false,
            settings_source_count: 0,
            profile: None,
            model: None,
            model_source: None,
            model_configured: false,
            credential_present: None,
            policy: None,
            policy_source: None,
            sandbox_enforced: None,
            configured_writable_count: None,
            configured_read_only_count: None,
        }
    }
}

/// The complete grouped report.
pub(super) struct DoctorReport {
    pub(super) summary: DoctorSummary,
    pub(super) groups: Vec<DoctorGroup>,
    pub(super) not_checked: Vec<GroupId>,
}

impl DoctorReport {
    /// Project the non-ok checks into findings, in report order.
    pub(super) fn findings(&self) -> Vec<DiagnosticCheck> {
        let mut findings = Vec::new();
        for group in &self.groups {
            for check in &group.checks {
                if check.status == DiagnosticStatus::Ok {
                    continue;
                }
                let mut finding =
                    DiagnosticCheck::new(check.id, check.status, check.message.clone())
                        .with_group(group.id.id());
                if let Some(fix) = &check.fix {
                    finding = finding.with_fix(fix.clone());
                }
                findings.push(finding);
            }
        }
        findings
    }

    /// Derive every count from the checks so the summary cannot drift.
    pub(super) fn from_groups(
        mut summary: DoctorSummary,
        groups: Vec<DoctorGroup>,
        not_checked: Vec<GroupId>,
    ) -> Self {
        let mut ok_count = 0;
        let mut warning_count = 0;
        let mut error_count = 0;
        for check in groups.iter().flat_map(|group| &group.checks) {
            match check.status {
                DiagnosticStatus::Ok => ok_count += 1,
                DiagnosticStatus::Warning => warning_count += 1,
                DiagnosticStatus::Error => error_count += 1,
            }
        }
        summary.ok_count = ok_count;
        summary.warning_count = warning_count;
        summary.error_count = error_count;
        summary.check_count = ok_count + warning_count + error_count;
        summary.incomplete = !not_checked.is_empty();
        Self {
            summary,
            groups,
            not_checked,
        }
    }
}
