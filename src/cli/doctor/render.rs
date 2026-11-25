//! Text and JSON projections of the `cake doctor` report model.
//!
//! Both renderers consume a completed [`DoctorReport`] and consult no terminal,
//! clock, filesystem, or environment.

use serde_json::Value;

use crate::cli::{DiagnosticDocument, DiagnosticStatus};

use crate::cli::doctor::report::{DoctorCheck, DoctorReport, DoctorSummary};

const LABEL_WIDTH: usize = 20;
const LOCAL_ONLY_NOTE: &str =
    "Local checks only; no provider, MCP server, hook, gate, or session process was started.";

/// Render the grouped text report with totals and a footer.
pub(super) fn render_text(report: &DoctorReport) -> String {
    let mut output = format!("Workspace  {}\n\n", report.summary.workspace);
    for (index, group) in report.groups.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(group.title);
        output.push('\n');
        for check in &group.checks {
            append_check(&mut output, check);
        }
    }
    if !report.groups.is_empty() {
        output.push('\n');
    }
    output.push_str(&totals(&report.summary));
    output.push('\n');
    if report.not_checked.is_empty() {
        output.push_str(LOCAL_ONLY_NOTE);
    } else {
        output.push_str("Not checked: ");
        let titles: Vec<&str> = report
            .not_checked
            .iter()
            .map(|group| group.title())
            .collect();
        output.push_str(&titles.join(", "));
    }
    output.push('\n');
    output
}

/// Render one diagnostic document, adding the grouped report to the payload.
pub(super) fn render_json(report: &DoctorReport, data: Value) -> anyhow::Result<String> {
    let Value::Object(mut data) = data else {
        anyhow::bail!("doctor data must be a JSON object");
    };
    data.insert("groups".to_string(), serde_json::to_value(&report.groups)?);
    data.insert(
        "not_checked".to_string(),
        serde_json::to_value(&report.not_checked)?,
    );
    DiagnosticDocument::new(
        "doctor",
        serde_json::to_value(&report.summary)?,
        report.findings(),
        Value::Object(data),
    )
    .render()
}

fn append_check(output: &mut String, check: &DoctorCheck) {
    let glyph = match check.status {
        DiagnosticStatus::Ok => '✓',
        DiagnosticStatus::Warning => '!',
        DiagnosticStatus::Error => '✗',
    };
    let gap = LABEL_WIDTH
        .saturating_sub(check.label.chars().count())
        .max(1);
    output.push_str("  ");
    output.push(glyph);
    output.push(' ');
    output.push_str(check.label);
    for _ in 0..gap {
        output.push(' ');
    }
    output.push_str(&check.message);
    output.push('\n');
    if let Some(fix) = &check.fix {
        output.push_str("      fix: ");
        output.push_str(&fix.summary);
        output.push('\n');
    }
}

fn totals(summary: &DoctorSummary) -> String {
    format!(
        "{} ok · {} · {}",
        summary.ok_count,
        counted(summary.warning_count, "warning"),
        counted(summary.error_count, "error")
    )
}

fn counted(count: usize, singular: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {singular}s")
    }
}
