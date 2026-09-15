use crate::cli::{
    CHECK_SETTINGS_LOAD_FAILED, CmdRunner, CommandRunOptions, DiagnosticDocument, report_failure,
    settings_warnings,
};
use crate::config::model::ApiType;
use crate::config::settings::LoadedSettings;
use crate::config::skills::{discover_skills_with_paths, parse_skill_path_list};
use crate::config::{DataDir, ModelDefinition, SettingsLoader};
use clap::{Parser, Subcommand};

/// Debug and introspection commands.
#[derive(Clone, Debug, Parser)]
#[command(after_help = "\
Examples:
  cake debug skills --catalog-budget 4000
  cake debug models --json | jq '.data.models[].name'")]
pub struct DebugCommand {
    #[command(subcommand)]
    command: DebugSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
enum DebugSubcommand {
    /// Report selected skill catalog size without calling a model
    Skills {
        /// Advisory rendered XML budget in Unicode characters
        #[arg(long, default_value_t = 8000)]
        catalog_budget: usize,
        /// Advisory routing-description budget in Unicode characters
        #[arg(long, default_value_t = 400)]
        description_budget: usize,
        /// Report an empty catalog, as for a run with --no-skills
        #[arg(long)]
        no_skills: bool,
        /// Only include these comma-separated skill names
        #[arg(long, value_name = "NAMES")]
        skills: Option<String>,
    },
    /// Show configured models from settings.toml
    #[command(after_help = "\
Examples:
  cake debug models
  cake debug models --json | jq '.data.models[].name'")]
    Models {
        /// Output model definitions as one diagnostic JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

impl CmdRunner for DebugCommand {
    async fn run(
        &self,
        _data_dir: &DataDir,
        options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<()> {
        // CmdRunner is asynchronous, although this command has no async work.
        std::future::ready(()).await;
        match &self.command {
            DebugSubcommand::Skills {
                catalog_budget,
                description_budget,
                no_skills,
                skills,
            } => run_skills(
                *catalog_budget,
                *description_budget,
                *no_skills,
                skills.as_deref(),
                options.profile,
            ),
            DebugSubcommand::Models { json } => run_models(*json),
        }
    }
}

/// Inspect configured models without setting up an agent session.
///
/// In machine mode the document carries the configured models and the settings
/// findings, so stderr stays quiet; without `--json` the warnings keep their
/// existing stderr behavior.
fn run_models(json: bool) -> anyhow::Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("Failed to get current directory: {e}"))?;
    let loaded = SettingsLoader::load(Some(&current_dir)).map_err(|error| {
        report_failure(
            json,
            "debug models",
            CHECK_SETTINGS_LOAD_FAILED,
            error.into(),
        )
    })?;
    print!("{}", render_models(&loaded, json)?);
    Ok(())
}

/// Render the configured models as the formatted table, or as one diagnostic
/// document when the machine flag is set.
fn render_models(loaded: &LoadedSettings, json: bool) -> anyhow::Result<String> {
    if json {
        render_models_json(loaded)
    } else {
        loaded.print_warnings();
        Ok(format_models(&loaded.models))
    }
}

/// Inspect the selected skill catalog without setting up an agent session.
fn run_skills(
    catalog_budget: usize,
    description_budget: usize,
    no_skills: bool,
    skills: Option<&str>,
    profile: Option<&str>,
) -> anyhow::Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("Failed to get current directory: {e}"))?;
    let loaded = SettingsLoader::load_with_profile(Some(&current_dir), profile)?;
    loaded.print_warnings();
    let roots = loaded
        .skills
        .path
        .as_deref()
        .map(parse_skill_path_list)
        .unwrap_or_default();
    let config = SettingsLoader::resolve_skill_config(no_skills, skills, &loaded.skills);
    let catalog = config.apply(discover_skills_with_paths(&current_dir, &roots));
    for diagnostic in &catalog.diagnostics {
        eprintln!(
            "Skill diagnostic ({}): {}",
            diagnostic.file.display(),
            diagnostic.message
        );
    }
    print!(
        "{}",
        catalog.size_report(catalog_budget, description_budget)
    );
    Ok(())
}

fn format_models(models: &std::collections::HashMap<String, ModelDefinition>) -> String {
    if models.is_empty() {
        return "No models configured.\n".to_string();
    }

    let mut rows: Vec<ModelRow<'_>> = models.values().map(ModelRow::from).collect();
    rows.sort_by(|left, right| left.name.cmp(right.name));

    let headers = ["Name", "Model", "API Type", "Base URL", "API Key Env"];
    let widths = column_widths(&headers, &rows);

    let mut output = String::new();
    output.push_str("Configured Models\n");
    output.push_str(&separator(&widths));
    output.push('\n');
    output.push_str(&format_row(&headers, &widths));
    output.push('\n');
    output.push_str(&separator(&widths));
    output.push('\n');

    for row in rows {
        output.push_str(&format_row(
            &[
                row.name,
                row.model,
                row.api_type,
                row.base_url,
                row.api_key_env,
            ],
            &widths,
        ));
        output.push('\n');
    }

    output.push_str(&separator(&widths));
    output.push('\n');
    output
}

struct ModelRow<'a> {
    name: &'a str,
    model: &'a str,
    api_type: &'static str,
    base_url: &'a str,
    api_key_env: &'a str,
}

impl<'a> From<&'a ModelDefinition> for ModelRow<'a> {
    fn from(definition: &'a ModelDefinition) -> Self {
        Self {
            name: definition.name.as_str(),
            model: definition.model.as_str(),
            api_type: api_type_label(definition.api_type),
            base_url: definition.base_url.as_str(),
            api_key_env: definition.api_key_env.as_str(),
        }
    }
}

const fn api_type_label(api_type: ApiType) -> &'static str {
    match api_type {
        ApiType::ChatCompletions => "chat_completions",
        ApiType::Responses => "responses",
    }
}

fn column_widths(headers: &[&str; 5], rows: &[ModelRow<'_>]) -> [usize; 5] {
    let mut widths = headers.map(str::len);
    for row in rows {
        let values = [
            row.name,
            row.model,
            row.api_type,
            row.base_url,
            row.api_key_env,
        ];
        for (index, value) in values.iter().enumerate() {
            widths[index] = widths[index].max(value.len());
        }
    }
    widths
}

fn format_row(values: &[&str; 5], widths: &[usize; 5]) -> String {
    format!(
        "{:<name_width$}  {:<model_width$}  {:<api_width$}  {:<url_width$}  {:<key_width$}",
        values[0],
        values[1],
        values[2],
        values[3],
        values[4],
        name_width = widths[0],
        model_width = widths[1],
        api_width = widths[2],
        url_width = widths[3],
        key_width = widths[4],
    )
}

/// Render the configured models and settings findings as one diagnostic
/// document.
fn render_models_json(loaded: &LoadedSettings) -> anyhow::Result<String> {
    let models = sorted_models(&loaded.models);
    DiagnosticDocument::new(
        "debug models",
        serde_json::json!({ "count": models.len() }),
        settings_warnings(&loaded.warnings),
        serde_json::json!({ "models": models }),
    )
    .render()
}

/// The configured models in name order, independent of map iteration order.
fn sorted_models(
    models: &std::collections::HashMap<String, ModelDefinition>,
) -> Vec<&ModelDefinition> {
    let mut defs: Vec<&ModelDefinition> = models.values().collect();
    defs.sort_by(|left, right| left.name.cmp(&right.name));
    defs
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

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str, api_type: ApiType) -> ModelDefinition {
        ModelDefinition {
            name: name.to_string(),
            model: format!("provider/{name}"),
            base_url: format!("https://{name}.example.com/v1"),
            api_key_env: format!("{}_API_KEY", name.to_ascii_uppercase()),
            provider: None,
            provider_headers: None,
            api_type,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            context_window: None,
            reasoning_effort: None,
            reasoning_summary: None,
            reasoning_max_tokens: None,
            providers: vec![],
        }
    }

    #[test]
    fn format_models_prints_sorted_model_rows() {
        let mut models = std::collections::HashMap::new();
        models.insert("zen".to_string(), model("zen", ApiType::ChatCompletions));
        models.insert("alpha".to_string(), model("alpha", ApiType::Responses));

        let output = format_models(&models);

        assert!(output.contains("Configured Models"));
        assert!(output.contains("Name"));
        assert!(output.contains("API Key Env"));
        assert!(output.contains("alpha  provider/alpha"));
        assert!(output.contains("responses"));
        assert!(output.contains("zen    provider/zen"));
        assert!(output.contains("chat_completions"));
        assert!(output.contains("ZEN_API_KEY"));
        assert!(!output.contains("actual-secret-value"));
        assert!(output.find("alpha").expect("alpha row") < output.find("zen").expect("zen row"));
    }

    #[test]
    fn format_models_handles_empty_settings() {
        let output = format_models(&std::collections::HashMap::new());
        assert_eq!(output, "No models configured.\n");
    }

    #[test]
    fn render_models_json_returns_a_diagnostic_document() {
        let mut models = std::collections::HashMap::new();
        models.insert("zen".to_string(), model("zen", ApiType::ChatCompletions));
        let loaded = loaded_settings(models, Vec::new());

        let output = render_models_json(&loaded).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["command"], "debug models");
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["summary"]["count"], 1);
        assert_eq!(parsed["checks"], serde_json::json!([]));
        assert_eq!(parsed["data"]["models"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["data"]["models"][0]["name"], "zen");
    }

    #[test]
    fn render_models_json_empty() {
        let output = render_models_json(&loaded_settings(
            std::collections::HashMap::default(),
            Vec::new(),
        ))
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["summary"]["count"], 0);
        assert_eq!(parsed["data"]["models"], serde_json::json!([]));
    }

    #[test]
    fn render_models_json_non_empty() {
        let mut models = std::collections::HashMap::new();
        models.insert("zen".to_string(), model("zen", ApiType::ChatCompletions));
        models.insert("alpha".to_string(), model("alpha", ApiType::Responses));

        let output = render_models_json(&loaded_settings(models, Vec::new())).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let arr = parsed["data"]["models"]
            .as_array()
            .expect("models must be a JSON array");
        assert_eq!(arr.len(), 2);

        // Verify sorted order (alpha before zen)
        assert_eq!(arr[0]["name"], "alpha");
        assert_eq!(arr[0]["model"], "provider/alpha");
        assert_eq!(arr[0]["api_type"], "responses");
        assert_eq!(arr[1]["name"], "zen");
        assert_eq!(arr[1]["model"], "provider/zen");
        assert_eq!(arr[1]["api_type"], "chat_completions");

        // Verify full fields are present (not just the table subset)
        assert!(arr[0].get("provider").is_some());
        assert!(arr[0].get("temperature").is_some());
        assert!(arr[0].get("top_p").is_some());

        // Verify no leaked secrets — only env var names
        assert_eq!(arr[0]["api_key_env"], "ALPHA_API_KEY");
        assert_eq!(arr[1]["api_key_env"], "ZEN_API_KEY");
    }

    #[test]
    fn render_models_json_reports_settings_findings_instead_of_stderr_text() {
        let warnings = vec!["unknown key 'temparature' in .cake/settings.toml".to_string()];
        let output = render_models_json(&loaded_settings(
            std::collections::HashMap::default(),
            warnings,
        ))
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed["status"], "warning");
        let checks = parsed["checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["id"], "settings.unknown_key");
        assert_eq!(checks[0]["status"], "warning");
        assert_eq!(
            checks[0]["message"],
            "unknown key 'temparature' in .cake/settings.toml"
        );
    }

    /// A loaded settings value with no models, for `--json` rendering tests.
    fn loaded_settings(
        models: std::collections::HashMap<String, ModelDefinition>,
        warnings: Vec<String>,
    ) -> LoadedSettings {
        LoadedSettings {
            models,
            default_model: None,
            directories: Vec::new(),
            sandbox: crate::config::settings::SandboxSettings::default(),
            skills: crate::config::settings::SkillSettings::default(),
            tools_enabled: None,
            system_prompt: None,
            judge: crate::config::settings::JudgeSettings::default(),
            limits: crate::config::settings::ResolvedLimits::default(),
            warnings,
        }
    }
}
