use crate::cli::CmdRunner;
use crate::config::model::ApiType;
use crate::config::{DataDir, ModelDefinition, SettingsLoader};
use clap::{Parser, Subcommand};

/// Debug and introspection commands.
#[derive(Clone, Debug, Parser)]
pub struct DebugCommand {
    #[command(subcommand)]
    command: DebugSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
enum DebugSubcommand {
    /// Show configured models from settings.toml
    Models {
        /// Output model definitions as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

impl CmdRunner for DebugCommand {
    async fn run(&self, _data_dir: &DataDir) -> anyhow::Result<()> {
        match &self.command {
            DebugSubcommand::Models { json } => {
                let current_dir = std::env::current_dir()
                    .map_err(|e| anyhow::anyhow!("Failed to get current directory: {e}"))?;
                let loaded = SettingsLoader::load(Some(&current_dir))?;
                print!("{}", render_models(&loaded.models, *json)?);
                Ok(())
            },
        }
    }
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

/// Render the configured models as either a JSON document or a formatted table.
fn render_models(
    models: &std::collections::HashMap<String, ModelDefinition>,
    json: bool,
) -> anyhow::Result<String> {
    if json {
        format_models_json(models)
    } else {
        Ok(format_models(models))
    }
}

fn format_models_json(
    models: &std::collections::HashMap<String, ModelDefinition>,
) -> anyhow::Result<String> {
    let mut defs: Vec<&ModelDefinition> = models.values().collect();
    defs.sort_by(|left, right| left.name.cmp(&right.name));
    serde_json::to_string_pretty(&defs)
        .map_err(|e| anyhow::anyhow!("Failed to serialize model definitions: {e}"))
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
    fn format_models_handles_optional_fields_unset() {
        let mut models = std::collections::HashMap::new();
        models.insert("zen".to_string(), model("zen", ApiType::ChatCompletions));

        let output = format_models(&models);

        assert!(output.contains("zen"));
        assert!(output.contains("provider/zen"));
        assert!(output.contains("https://zen.example.com/v1"));
    }

    #[test]
    fn render_models_json_true_returns_json_array() {
        let mut models = std::collections::HashMap::new();
        models.insert("zen".to_string(), model("zen", ApiType::ChatCompletions));

        let output = render_models(&models, true).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed.as_array().expect("JSON array").len(), 1);
        assert_eq!(parsed[0]["name"], "zen");
    }

    #[test]
    fn render_models_json_false_returns_table() {
        let mut models = std::collections::HashMap::new();
        models.insert("zen".to_string(), model("zen", ApiType::ChatCompletions));

        let output = render_models(&models, false).unwrap();

        assert!(output.contains("Configured Models"));
        assert!(output.contains("provider/zen"));
    }

    #[test]
    fn format_models_json_empty() {
        let output = format_models_json(&std::collections::HashMap::new()).unwrap();
        assert_eq!(output, "[]");
    }

    #[test]
    fn format_models_json_non_empty() {
        let mut models = std::collections::HashMap::new();
        models.insert("zen".to_string(), model("zen", ApiType::ChatCompletions));
        models.insert("alpha".to_string(), model("alpha", ApiType::Responses));

        let output = format_models_json(&models).unwrap();

        // Parse and verify it's valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let arr = parsed.as_array().expect("output must be a JSON array");
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
}
