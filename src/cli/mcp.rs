//! `cake mcp` management for user-owned MCP server definitions.
//!
//! `add` and `remove` edit only the global `<config>/cake/mcp.json` file.
//! `list` and `get` resolve the same sources a run uses (the global file plus
//! a trusted project file) without starting a server. Every command reports
//! environment variable names, never values.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use clap::{ArgGroup, Parser, Subcommand};
use serde::Serialize;
use serde_json::json;

use crate::cli::settings::load_settings;
use crate::cli::{
    CHECK_MCP_CONFIG, CHECK_MCP_NOT_FOUND, CmdRunner, CommandRunOptions, DiagnosticCheck,
    DiagnosticDocument, report_failure,
};
use crate::config::DataDir;
use crate::config::mcp::{
    self, DEFAULT_MCP_TIMEOUT_SECS, McpConfigLoad, McpManageError, McpServerConfig, McpServerDraft,
    valid_env_name,
};

/// Manage configured MCP server definitions.
#[derive(Clone, Debug, Parser)]
#[command(after_help = "\
Examples:
  cake mcp list
  cake mcp add safari-mcp -- /usr/bin/safaridriver --mcp
  cake mcp add fixture --env FAKE_MCP_ERA=modern -- python3 server.py
  cake mcp get safari-mcp --json
  cake mcp remove safari-mcp")]
pub struct McpCommand {
    #[command(subcommand)]
    command: McpSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
enum McpSubcommand {
    /// List resolved MCP server definitions without starting them
    #[command(after_help = "\
Examples:
  cake mcp list
  cake mcp list --json | jq '.data.servers[].name'")]
    List {
        /// Output the server list as one diagnostic JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Show one resolved MCP server definition
    #[command(after_help = "\
Examples:
  cake mcp get safari-mcp
  cake mcp get safari-mcp --json")]
    Get {
        /// Server name to display
        #[arg(value_name = "NAME")]
        name: String,
        /// Output the server as one diagnostic JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Add or replace a stdio server in the global MCP config file
    #[command(override_usage = "cake mcp add [OPTIONS] <NAME> (--url <URL> | -- <COMMAND>...)")]
    #[command(group(
        ArgGroup::new("transport")
            .args(["command", "url"])
            .required(true)
            .multiple(false)
    ))]
    Add(AddArgs),
    /// Remove a stdio server from the global MCP config file
    #[command(after_help = "\
Examples:
  cake mcp remove safari-mcp")]
    Remove {
        /// Server name to remove
        #[arg(value_name = "NAME")]
        name: String,
    },
}

/// `cake mcp add` arguments.
#[derive(Clone, Debug, Parser)]
struct AddArgs {
    /// Local name for the server; becomes part of mcp__<name>__<tool>
    #[arg(value_name = "NAME")]
    name: String,
    /// Remote streamable HTTP URL (not supported by Cake)
    #[arg(long, value_name = "URL")]
    url: Option<String>,
    /// Environment variable for the server process; repeatable
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_env_pair)]
    env: Vec<(String, String)>,
    /// Per-request timeout in seconds (default 60, maximum 3600)
    #[arg(long, value_name = "SECONDS")]
    timeout_secs: Option<u64>,
    /// Executable and arguments to launch the server; provide after `--`
    #[arg(trailing_var_arg = true, num_args = 0.., value_name = "COMMAND")]
    command: Vec<String>,
}

impl CmdRunner for McpCommand {
    async fn run(
        &self,
        _data_dir: &DataDir,
        options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<()> {
        // CmdRunner is asynchronous, although this command has no async work.
        std::future::ready(()).await;
        match &self.command {
            McpSubcommand::List { json } => list_command(*json, options),
            McpSubcommand::Get { name, json } => get_command(name, *json, options),
            McpSubcommand::Add(args) => add_command(args),
            McpSubcommand::Remove { name } => remove_command(name),
        }
    }
}

/// Parse one repeatable `--env KEY=VALUE` assignment.
fn parse_env_pair(value: &str) -> Result<(String, String), String> {
    let Some((key, env_value)) = value.split_once('=') else {
        return Err("expected KEY=VALUE".to_string());
    };
    if !valid_env_name(key) {
        return Err(
            "the key must start with a letter or '_' and contain only ASCII letters, digits, \
             and '_'"
                .to_string(),
        );
    }
    if env_value.contains('\0') {
        return Err("the value may not contain a NUL byte".to_string());
    }
    Ok((key.to_string(), env_value.to_string()))
}

/// Resolve the global MCP configuration path for this invocation.
fn global_path() -> std::path::PathBuf {
    mcp::global_config_path(&crate::config::config_dir())
}

/// Add or replace one server in the global file.
fn add_command(args: &AddArgs) -> anyhow::Result<()> {
    if args.url.is_some() {
        return Err(McpManageError::UnsupportedUrl {
            name: args.name.clone(),
        }
        .into());
    }
    let Some(command) = args.command.first() else {
        return Err(McpManageError::InvalidCommand {
            name: args.name.clone(),
        }
        .into());
    };
    let draft = McpServerDraft {
        name: args.name.clone(),
        command: command.clone(),
        args: args.command.iter().skip(1).cloned().collect(),
        env: args.env.iter().cloned().collect::<BTreeMap<_, _>>(),
        timeout_secs: args.timeout_secs.unwrap_or(DEFAULT_MCP_TIMEOUT_SECS),
    };
    let path = global_path();
    mcp::upsert_server(&path, &draft)?;
    println!(
        "Added global MCP server '{}' to {}.",
        draft.name,
        path.display()
    );
    Ok(())
}

/// Remove one server from the global file.
fn remove_command(name: &str) -> anyhow::Result<()> {
    let path = global_path();
    if mcp::remove_server(&path, name)? {
        println!("Removed global MCP server '{name}'.");
    } else {
        println!(
            "No global MCP server named '{name}' found in {}.",
            path.display()
        );
    }
    Ok(())
}

/// List the resolved definitions without starting a server.
fn list_command(json_output: bool, options: &CommandRunOptions<'_>) -> anyhow::Result<()> {
    let discovery = resolve_definitions("mcp list", json_output, options)?;
    let entries: Vec<McpEntry> = discovery.servers.iter().map(McpEntry::from).collect();
    if json_output {
        DiagnosticDocument::new(
            "mcp list",
            json!({ "count": entries.len() }),
            warning_checks(&discovery.warnings),
            json!({ "servers": entries }),
        )
        .print();
        return Ok(());
    }
    print_warnings(&discovery.warnings);
    if entries.is_empty() {
        println!("No MCP servers configured. Try `cake mcp add <name> -- <command>`.");
        return Ok(());
    }
    let mut output = String::from("MCP servers\n");
    for entry in &entries {
        output.push('\n');
        format_entry(entry, &mut output);
    }
    print!("{output}");
    Ok(())
}

/// Show one resolved definition.
fn get_command(
    name: &str,
    json_output: bool,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    let discovery = resolve_definitions("mcp get", json_output, options)?;
    if !json_output {
        print_warnings(&discovery.warnings);
    }
    let Some(server) = discovery.servers.iter().find(|server| server.name == name) else {
        let error = McpManageError::NotFound {
            name: name.to_string(),
        };
        if json_output {
            let mut checks = warning_checks(&discovery.warnings);
            checks.push(DiagnosticCheck::error(
                CHECK_MCP_NOT_FOUND,
                error.to_string(),
            ));
            DiagnosticDocument::new("mcp get", json!({ "name": name }), checks, json!({})).print();
        }
        return Err(error.into());
    };
    let entry = McpEntry::from(server);
    if json_output {
        DiagnosticDocument::new(
            "mcp get",
            json!({ "name": name }),
            warning_checks(&discovery.warnings),
            json!({ "server": entry }),
        )
        .print();
        return Ok(());
    }
    let mut output = String::new();
    format_entry(&entry, &mut output);
    print!("{output}");
    Ok(())
}

/// Resolve the same server sources a run uses, without connecting.
fn resolve_definitions(
    command: &'static str,
    json_output: bool,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<McpConfigLoad> {
    let current_dir = std::env::current_dir()
        .map_err(|error| anyhow::anyhow!("Failed to get current directory: {error}"))?;
    let load = load_settings(command, json_output, options)?;
    mcp::discover_mcp_servers(
        &[],
        &current_dir,
        &crate::config::config_dir(),
        &current_dir,
        &load.trust,
        options.project_config_mode,
        load.mcp_config_bytes.as_deref(),
    )
    .map_err(|error| report_failure(json_output, command, CHECK_MCP_CONFIG, error.into()))
}

/// Turn discovery warnings into document findings.
fn warning_checks(warnings: &[String]) -> Vec<DiagnosticCheck> {
    warnings
        .iter()
        .map(|warning| DiagnosticCheck::warning(CHECK_MCP_CONFIG, warning.clone()))
        .collect()
}

/// Keep human diagnostics on stderr.
fn print_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

/// One resolved server in list and get output.
#[derive(Debug, Serialize)]
struct McpEntry {
    name: String,
    command: String,
    args: Vec<String>,
    env: Vec<String>,
    timeout_secs: u64,
    source: String,
}

impl From<&McpServerConfig> for McpEntry {
    fn from(config: &McpServerConfig) -> Self {
        Self {
            name: config.name.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            env: config.env.keys().cloned().collect(),
            timeout_secs: config.timeout_secs,
            source: config.source.display().to_string(),
        }
    }
}

/// Render one server block. Environment values are never shown.
fn format_entry(entry: &McpEntry, output: &mut String) {
    _ = writeln!(output, "{}", entry.name);
    _ = writeln!(output, "  source: {}", entry.source);
    let command = std::iter::once(entry.command.as_str())
        .chain(entry.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    _ = writeln!(output, "  command: {command}");
    if !entry.env.is_empty() {
        _ = writeln!(output, "  env: {}", entry.env.join(", "));
    }
    _ = writeln!(output, "  timeout: {}s", entry.timeout_secs);
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod mcp_tests;
