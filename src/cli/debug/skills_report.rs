//! Human-readable and diagnostic JSON reports for `cake debug skills`.

use crate::cli::{
    CHECK_SKILLS_DISCOVERY, CHECK_SKILLS_PROJECT_TRUST, DiagnosticCheck, DiagnosticDocument,
    settings_warnings,
};
use crate::config::skills::{SkillCatalog, SkillDiagnostic};
use crate::config::trust::ProjectTrustReport;
use serde::Serialize;
use serde_json::json;
use std::fmt::Write as _;

/// One selected skill row in the `debug skills` report.
#[derive(Debug, Serialize)]
struct SkillReportRow {
    name: String,
    description: String,
    scope: &'static str,
    location: String,
    base_directory: String,
    description_characters: usize,
    description_over_budget: bool,
}

/// Build the selected skill rows in name order, with location breaking ties.
fn skill_rows(catalog: &SkillCatalog, description_budget: usize) -> Vec<SkillReportRow> {
    let mut rows: Vec<SkillReportRow> = catalog
        .skills
        .iter()
        .map(|skill| {
            let description_characters = skill.description.chars().count();
            SkillReportRow {
                name: skill.name.clone(),
                description: skill.description.clone(),
                scope: skill.scope.label(),
                location: skill.location.display().to_string(),
                base_directory: skill.base_directory.display().to_string(),
                description_characters,
                description_over_budget: description_characters > description_budget,
            }
        })
        .collect();
    rows.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.location.cmp(&right.location))
    });
    rows
}

/// Skill diagnostics in deterministic file/severity/message order.
fn sorted_skill_diagnostics(catalog: &SkillCatalog) -> Vec<&SkillDiagnostic> {
    let mut diagnostics: Vec<&SkillDiagnostic> = catalog.diagnostics.iter().collect();
    diagnostics.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then_with(|| left.level.label().cmp(right.level.label()))
            .then_with(|| left.message.cmp(&right.message))
    });
    diagnostics
}

/// One text line for a skill diagnostic, with the file omitted when absent.
pub(super) fn format_skill_diagnostic(diagnostic: &SkillDiagnostic) -> String {
    if diagnostic.file.as_os_str().is_empty() {
        format!("{}: {}", diagnostic.level.label(), diagnostic.message)
    } else {
        format!(
            "{}: {}: {}",
            diagnostic.level.label(),
            diagnostic.file.display(),
            diagnostic.message
        )
    }
}

/// Skill diagnostics as non-fatal findings: the catalog still renders.
fn skill_diagnostic_checks(catalog: &SkillCatalog) -> Vec<DiagnosticCheck> {
    sorted_skill_diagnostics(catalog)
        .into_iter()
        .map(|diagnostic| {
            DiagnosticCheck::warning(CHECK_SKILLS_DISCOVERY, format_skill_diagnostic(diagnostic))
        })
        .collect()
}

/// Render the human-readable selected catalog report.
pub(super) fn format_text(
    catalog: &SkillCatalog,
    catalog_budget: usize,
    description_budget: usize,
) -> String {
    let rows = skill_rows(catalog, description_budget);
    let metrics = catalog.metrics();
    let descriptions_over_budget = rows
        .iter()
        .filter(|row| row.description_over_budget)
        .count();

    let mut output = String::from("Skill catalog\n");
    if rows.is_empty() {
        output.push_str("No skills selected.\n");
    } else if rows.len() == 1 {
        output.push_str("Selected: 1 skill\n");
    } else {
        _ = writeln!(output, "Selected: {} skills", rows.len());
    }
    _ = writeln!(
        output,
        "Rendered catalog: {} bytes, {} characters, ~{} tokens (ceil(characters / 4); not a provider token count).",
        metrics.bytes, metrics.characters, metrics.estimated_tokens
    );
    _ = writeln!(
        output,
        "Advisory budgets: {catalog_budget} catalog characters; {description_budget} description characters."
    );
    if metrics.catalog_over_budget(catalog_budget) {
        _ = writeln!(
            output,
            "Catalog budget: WARNING: rendered catalog exceeds the advisory budget by {} characters.",
            metrics.catalog_over_budget_by(catalog_budget)
        );
    } else {
        output.push_str("Catalog budget: within budget\n");
    }
    _ = writeln!(
        output,
        "Descriptions over budget: {descriptions_over_budget}"
    );

    if !rows.is_empty() {
        output.push('\n');
        append_skill_table(&mut output, &rows);
    }

    output.push('\n');
    append_skill_diagnostics(&mut output, catalog);
    output
}

/// Append the fixed-width skill inventory table.
fn append_skill_table(output: &mut String, rows: &[SkillReportRow]) {
    let headers = ["Name", "Scope", "Desc chars", "Budget", "Location"];
    let widths = skill_column_widths(&headers, rows);
    output.push_str(format_skill_row(&headers, &widths).trim_end());
    output.push('\n');
    output.push_str(&separator(&widths));
    output.push('\n');
    for row in rows {
        let characters = row.description_characters.to_string();
        let budget = if row.description_over_budget {
            "over"
        } else {
            "ok"
        };
        let line = format_skill_row(
            &[
                row.name.as_str(),
                row.scope,
                characters.as_str(),
                budget,
                row.location.as_str(),
            ],
            &widths,
        );
        output.push_str(line.trim_end());
        output.push('\n');
    }
}

fn skill_column_widths(headers: &[&str; 5], rows: &[SkillReportRow]) -> [usize; 5] {
    let mut widths = headers.map(str::len);
    for row in rows {
        widths[0] = widths[0].max(row.name.len());
        widths[1] = widths[1].max(row.scope.len());
        widths[2] = widths[2].max(row.description_characters.to_string().len());
        widths[3] = widths[3].max(if row.description_over_budget { 4 } else { 2 });
        widths[4] = widths[4].max(row.location.len());
    }
    widths
}

fn format_skill_row(values: &[&str; 5], widths: &[usize; 5]) -> String {
    format!(
        "{:<name_width$}  {:<scope_width$}  {:<characters_width$}  {:<budget_width$}  {:<location_width$}",
        values[0],
        values[1],
        values[2],
        values[3],
        values[4],
        name_width = widths[0],
        scope_width = widths[1],
        characters_width = widths[2],
        budget_width = widths[3],
        location_width = widths[4],
    )
}

fn separator(widths: &[usize; 5]) -> String {
    format!(
        "{}  {}  {}  {}  {}",
        "-".repeat(widths[0]),
        "-".repeat(widths[1]),
        "-".repeat(widths[2]),
        "-".repeat(widths[3]),
        "-".repeat(widths[4]),
    )
}

/// Append discovery and selection diagnostics to the text report.
fn append_skill_diagnostics(output: &mut String, catalog: &SkillCatalog) {
    let diagnostics = sorted_skill_diagnostics(catalog);
    if diagnostics.is_empty() {
        output.push_str("Diagnostics: none\n");
        return;
    }

    output.push_str("Diagnostics\n");
    for diagnostic in diagnostics {
        _ = writeln!(output, "{}", format_skill_diagnostic(diagnostic));
    }
}

/// Render the selected catalog and its findings as one diagnostic document.
pub(super) fn render_json(
    catalog: &SkillCatalog,
    warnings: &[String],
    trust: &ProjectTrustReport,
    catalog_budget: usize,
    description_budget: usize,
) -> anyhow::Result<String> {
    let rows = skill_rows(catalog, description_budget);
    let metrics = catalog.metrics();
    let count = rows.len();
    let descriptions_over_budget = rows
        .iter()
        .filter(|row| row.description_over_budget)
        .count();

    let mut checks = settings_warnings(warnings);
    if let Some(message) = super::project_trust_warning_message(trust) {
        checks.push(DiagnosticCheck::warning(
            CHECK_SKILLS_PROJECT_TRUST,
            message,
        ));
    }
    checks.extend(skill_diagnostic_checks(catalog));

    DiagnosticDocument::new(
        "debug skills",
        json!({
            "count": count,
            "catalog_budget": catalog_budget,
            "description_budget": description_budget,
            "rendered_bytes": metrics.bytes,
            "rendered_characters": metrics.characters,
            "estimated_tokens": metrics.estimated_tokens,
            "catalog_over_budget": metrics.catalog_over_budget(catalog_budget),
            "catalog_over_budget_characters": metrics.catalog_over_budget_by(catalog_budget),
            "descriptions_over_budget": descriptions_over_budget,
        }),
        checks,
        json!({
            "skills": rows,
            "project_trust": {
                "state": trust.state.label(),
                "authority": trust.authority,
                "changes": trust.changes,
            },
        }),
    )
    .render()
}
