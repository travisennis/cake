//! `cake debug grants`: effective filesystem grants and their sources.
//!
//! The report resolves inputs the same way a run does, but never provisions a
//! configured writable directory or the user skill root. Missing paths are
//! data, not grants.

use crate::cli::settings::load_settings;
use crate::cli::{
    CHECK_GRANTS_PATH, CommandRunOptions, DiagnosticCheck, DiagnosticDocument, settings_warnings,
};
use crate::clients::{
    GrantReport, GrantReportInputs, SandboxPolicy, compute_temp_directories,
    resolve_sandbox_policy_with_source,
};
use crate::config::settings::LoadedSettings;
use crate::config::skills::{discover_skills_with_paths, parse_skill_path_list};
use crate::config::trust::ProjectTrustReport;
use crate::config::{DataDir, SettingsLoader};
use serde_json::{Map, Value, json};
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::cli::debug::print_project_trust_warning;

/// Inspect the effective filesystem grants without creating an agent session.
pub(super) fn run_grants(
    sandbox: Option<SandboxPolicy>,
    add_dir: &[String],
    no_skills: bool,
    skills: Option<&str>,
    json: bool,
    data_dir: &DataDir,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    let load = load_settings("debug grants", json, options)?;
    let loaded = load.settings;
    let current_dir = std::env::current_dir()
        .map_err(|error| anyhow::anyhow!("Failed to get current directory: {error}"))?;
    let roots = loaded
        .skills
        .path
        .as_deref()
        .map(parse_skill_path_list)
        .unwrap_or_default();
    let config = SettingsLoader::resolve_skill_config(no_skills, skills, &loaded.skills);
    let catalog = config.apply(discover_skills_with_paths(&current_dir, &roots));
    let skill_dirs = catalog
        .skills
        .iter()
        .map(|skill| skill.base_directory.clone())
        .collect();
    let report = build_report(
        sandbox,
        add_dir,
        &loaded,
        skill_dirs,
        &current_dir,
        data_dir,
    );

    if json {
        print!("{}", render_json(&report, &loaded.warnings, &load.trust)?);
    } else {
        loaded.print_warnings();
        print_project_trust_warning(&load.trust);
        print!("{}", format_text(&report));
    }
    Ok(())
}

/// Resolve the same grant inputs a run uses, without provisioning anything.
///
/// Both `debug grants` and `debug config` call this function so their grants
/// section reports one resolved value rather than two near-copies.
pub fn build_report(
    sandbox: Option<SandboxPolicy>,
    add_dir: &[String],
    loaded: &LoadedSettings,
    skill_dirs: Vec<PathBuf>,
    current_dir: &std::path::Path,
    data_dir: &DataDir,
) -> GrantReport {
    let (policy, policy_source) = resolve_sandbox_policy_with_source(sandbox);
    GrantReport::build(&GrantReportInputs {
        policy,
        policy_source,
        cwd: current_dir.to_path_buf(),
        temp_dirs: compute_temp_directories(),
        directories: loaded.directories.clone(),
        writable: loaded.sandbox.writable.clone(),
        read_only: loaded.sandbox.read_only.clone(),
        add_dir: add_dir.to_vec(),
        skill_dirs,
        user_skill_root: crate::config::user_skills_dir(),
        data_read_dirs: data_dir.sandbox_read_dirs(),
        global_config_dir: crate::config::config_dir().join("cake"),
        home: std::env::var_os("HOME").map(PathBuf::from),
    })
}

/// Render the human-readable report.
pub(super) fn format_text(report: &GrantReport) -> String {
    let mut output = String::from("Effective grants\n");
    _ = writeln!(
        output,
        "Policy: {} (from {})",
        report.policy.label(),
        report.policy_source.label()
    );
    if report.policy.applies_os_sandbox() {
        output.push_str("OS sandbox: applied\n");
    } else {
        output.push_str("OS sandbox: not applied (danger-full-access)\n");
    }
    _ = writeln!(
        output,
        "Sources: {} lists, {} paths",
        report.sources.len(),
        source_path_count(report)
    );
    _ = writeln!(
        output,
        "Effective sets: {} read-write, {} read-only, {} sandbox-writable, {} sandbox-only-read, {} denied, {} user grants",
        report.read_write.len(),
        report.read_only.len(),
        report.sandbox_writable.len(),
        report.sandbox_only_read.len(),
        report.denied.len(),
        report.user_grants.len()
    );

    output.push('\n');
    append_source_table(&mut output, report);
    output.push('\n');
    append_effective_sets(&mut output, report);
    output.push('\n');
    append_projections(&mut output, report);
    output.push('\n');
    append_diagnostics(&mut output, report);
    output
}

/// Render the report as one versioned diagnostic document.
pub(super) fn render_json(
    report: &GrantReport,
    settings_warnings_json: &[String],
    trust: &ProjectTrustReport,
) -> anyhow::Result<String> {
    let mut checks = settings_warnings(settings_warnings_json);
    checks.extend(
        report
            .warnings()
            .into_iter()
            .map(|warning| DiagnosticCheck::warning(CHECK_GRANTS_PATH, warning)),
    );

    let mut summary = summary(report);
    summary.insert("warning_count".to_string(), json!(checks.len()));
    DiagnosticDocument::new(
        "debug grants",
        Value::Object(summary),
        checks,
        data_with_trust(report, trust),
    )
    .render()
}

/// The scalar grant fields, without the command envelope.
pub(super) fn summary(report: &GrantReport) -> Map<String, Value> {
    let mut summary = Map::new();
    summary.insert("policy".to_string(), json!(report.policy.label()));
    summary.insert(
        "policy_source".to_string(),
        json!(report.policy_source.label()),
    );
    summary.insert(
        "sandbox_enforced".to_string(),
        json!(report.policy.applies_os_sandbox()),
    );
    summary.insert("source_count".to_string(), json!(report.sources.len()));
    summary.insert(
        "source_path_count".to_string(),
        json!(source_path_count(report)),
    );
    summary.insert(
        "read_write_count".to_string(),
        json!(report.read_write.len()),
    );
    summary.insert("read_only_count".to_string(), json!(report.read_only.len()));
    summary.insert(
        "sandbox_writable_count".to_string(),
        json!(report.sandbox_writable.len()),
    );
    summary.insert(
        "sandbox_only_read_count".to_string(),
        json!(report.sandbox_only_read.len()),
    );
    summary.insert("denied_count".to_string(), json!(report.denied.len()));
    summary.insert(
        "user_grants_count".to_string(),
        json!(report.user_grants.len()),
    );
    summary.insert(
        "bash_writable_count".to_string(),
        json!(report.bash_writable.len()),
    );
    summary.insert(
        "bash_read_execute_count".to_string(),
        json!(report.bash_read_execute.len()),
    );
    summary
}

/// The grant payload and source lists, without the command envelope.
pub(super) fn data(report: &GrantReport) -> Value {
    let sources: Vec<Value> = report
        .sources
        .iter()
        .map(|source| {
            let entries: Vec<Value> = source
                .entries
                .iter()
                .map(|entry| {
                    json!({
                        "path": entry.path.display().to_string(),
                        "status": entry.status.label(),
                        "detail": entry.detail,
                    })
                })
                .collect();
            json!({
                "id": source.id,
                "label": source.label,
                "access": source.access.label(),
                "entries": entries,
                "notices": source.notices,
            })
        })
        .collect();

    json!({
        "sources": sources,
        "read_write": path_strings(&report.read_write),
        "read_only": path_strings(&report.read_only),
        "sandbox_writable": path_strings(&report.sandbox_writable),
        "sandbox_only_read": path_strings(&report.sandbox_only_read),
        "denied": report.denied.iter().map(|entry| json!({
            "path": entry.path.display().to_string(),
            "kind": entry.kind.label(),
        })).collect::<Vec<_>>(),
        "user_grants": path_strings(&report.user_grants),
        "projections": {
            "file_tools": "Read and ReadImage use read_write and read_only; Edit and Write use read_write only; denied wins first.",
            "bash_writable": path_strings(&report.bash_writable),
            "bash_read_execute": path_strings(&report.bash_read_execute),
        },
    })
}

/// The summary and data fields as one flat object for embedding.
pub(super) fn flat_payload(report: &GrantReport) -> Value {
    let mut payload = summary(report);
    if let Some(object) = data(report).as_object() {
        payload.extend(object.clone());
    }
    Value::Object(payload)
}

fn data_with_trust(report: &GrantReport, trust: &ProjectTrustReport) -> Value {
    let mut data = data(report);
    if let Some(object) = data.as_object_mut() {
        object.insert(
            "project_trust".to_string(),
            json!({
                "state": trust.state.label(),
                "authority": trust.authority,
                "changes": trust.changes,
            }),
        );
    }
    data
}

fn source_path_count(report: &GrantReport) -> usize {
    report
        .sources
        .iter()
        .map(|source| source.entries.len())
        .sum()
}

fn path_strings(paths: &[std::path::PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

struct SourceRow {
    source: String,
    access: String,
    path: String,
    status: String,
}

fn append_source_table(output: &mut String, report: &GrantReport) {
    let mut rows = Vec::new();
    for source in &report.sources {
        if source.entries.is_empty() {
            rows.push(SourceRow {
                source: source.label.to_string(),
                access: source.access.label().to_string(),
                path: "(none)".to_string(),
                status: "-".to_string(),
            });
            continue;
        }
        for (index, entry) in source.entries.iter().enumerate() {
            rows.push(SourceRow {
                source: if index == 0 {
                    source.label.to_string()
                } else {
                    String::new()
                },
                access: if index == 0 {
                    source.access.label().to_string()
                } else {
                    String::new()
                },
                path: entry.path.display().to_string(),
                status: entry.status.label().to_string(),
            });
        }
    }

    let headers = ["Source", "Access", "Path", "Status"];
    let widths = source_column_widths(&headers, &rows);
    output.push_str("Sources\n");
    output.push_str(format_source_row(&headers, &widths).trim_end());
    output.push('\n');
    output.push_str(&source_separator(&widths));
    output.push('\n');
    for row in rows {
        output.push_str(
            format_source_row(&[&row.source, &row.access, &row.path, &row.status], &widths)
                .trim_end(),
        );
        output.push('\n');
    }
}

fn source_column_widths(headers: &[&str; 4], rows: &[SourceRow]) -> [usize; 4] {
    let mut widths = headers.map(str::len);
    for row in rows {
        widths[0] = widths[0].max(row.source.len());
        widths[1] = widths[1].max(row.access.len());
        widths[2] = widths[2].max(row.path.len());
        widths[3] = widths[3].max(row.status.len());
    }
    widths
}

fn format_source_row(values: &[&str; 4], widths: &[usize; 4]) -> String {
    format!(
        "{:<source_width$}  {:<access_width$}  {:<path_width$}  {:<status_width$}",
        values[0],
        values[1],
        values[2],
        values[3],
        source_width = widths[0],
        access_width = widths[1],
        path_width = widths[2],
        status_width = widths[3],
    )
}

fn source_separator(widths: &[usize; 4]) -> String {
    format!(
        "{}  {}  {}  {}",
        "-".repeat(widths[0]),
        "-".repeat(widths[1]),
        "-".repeat(widths[2]),
        "-".repeat(widths[3]),
    )
}

fn append_effective_sets(output: &mut String, report: &GrantReport) {
    output.push_str("Effective sets\n");
    append_paths(output, "read_write", &report.read_write);
    append_paths(output, "read_only", &report.read_only);
    append_paths(output, "sandbox_writable", &report.sandbox_writable);
    append_paths(output, "sandbox_only_read", &report.sandbox_only_read);
    append_denied(output, report);
    append_paths(output, "user_grants", &report.user_grants);
}

fn append_paths(output: &mut String, title: &str, paths: &[std::path::PathBuf]) {
    _ = writeln!(output, "{title} ({})", paths.len());
    if paths.is_empty() {
        output.push_str("  (none)\n");
        return;
    }
    for path in paths {
        _ = writeln!(output, "  {}", path.display());
    }
}

fn append_denied(output: &mut String, report: &GrantReport) {
    _ = writeln!(output, "denied ({})", report.denied.len());
    if report.denied.is_empty() {
        output.push_str("  (none)\n");
        return;
    }
    for entry in &report.denied {
        _ = writeln!(
            output,
            "  {} ({})",
            entry.path.display(),
            entry.kind.label()
        );
    }
}

fn append_projections(output: &mut String, report: &GrantReport) {
    output.push_str("Projections\n");
    output.push_str(
        "  File tools: Read and ReadImage use read_write and read_only; Edit and Write use read_write only; denied wins first.\n",
    );
    _ = writeln!(
        output,
        "  Bash writable: read_write + sandbox_writable ({} paths)",
        report.bash_writable.len()
    );
    _ = writeln!(
        output,
        "  Bash read-execute: (read_only - sandbox_writable) + sandbox_only_read ({} paths)",
        report.bash_read_execute.len()
    );
    if !report.policy.applies_os_sandbox() {
        output.push_str(
            "  Bash: no OS sandbox is applied under danger-full-access; the effective sets still describe in-process grants.\n",
        );
    }
}

fn append_diagnostics(output: &mut String, report: &GrantReport) {
    let warnings = report.warnings();
    if warnings.is_empty() {
        output.push_str("Diagnostics: none\n");
        return;
    }
    output.push_str("Diagnostics\n");
    for warning in warnings {
        _ = writeln!(output, "warning: {warning}");
    }
}
