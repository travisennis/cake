//! Sanitized identity of the project context used to build a session request.
//!
//! The snapshot records paths, selections, and one-way digests only. Prompt
//! text, `AGENTS.md` contents, skill bodies, tool schemas, and MCP payloads are
//! never stored here. Resume compares a stored snapshot with the current
//! rebuild so a changed request prefix is explained instead of silently sent.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The system prompt source and a digest of the built prompt.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextSystemPromptIdentity {
    /// Stable source label: `cli`, `project`, `settings`, `user`, or `builtin`.
    pub source: String,
    /// Source file, absent for the built-in prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// SHA-256 of the built system prompt message.
    pub digest: String,
}

/// One instruction file and a digest of its content.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextFileIdentity {
    /// Display path used by instruction discovery.
    pub path: String,
    /// SHA-256 of the file content.
    pub digest: String,
}

/// One selected skill and a digest of its `SKILL.md` file.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextSkillIdentity {
    /// Skill name.
    pub name: String,
    /// Discovery scope: `project`, `configured`, or `user`.
    pub scope: String,
    /// Absolute `SKILL.md` path.
    pub location: PathBuf,
    /// SHA-256 of the `SKILL.md` file.
    pub digest: String,
}

/// One connected MCP server and the names it advertised.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextMcpServerIdentity {
    /// Local server name.
    pub name: String,
    /// Registered tool names, sorted.
    pub tools: Vec<String>,
}

/// MCP configuration sources and connected capabilities.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextMcpIdentity {
    /// Configuration files considered for this run, sorted.
    pub config_paths: Vec<PathBuf>,
    /// Connected servers, sorted by name.
    pub servers: Vec<ContextMcpServerIdentity>,
}

/// Request-affecting profile definition state.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextProfileIdentity {
    /// Selected behavior profile, absent when no profile was applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// SHA-256 of the resolved request-affecting settings.
    pub request_settings_digest: String,
}

/// Sanitized identity of the context rebuilt for one session request.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextIdentity {
    /// Directory the request context was rebuilt from.
    pub working_directory: PathBuf,
    /// Canonical form of `working_directory`.
    pub project_root: PathBuf,
    /// Selected system prompt source and digest.
    pub system_prompt: ContextSystemPromptIdentity,
    /// Instruction files in discovery order.
    pub agents_files: Vec<ContextFileIdentity>,
    /// SHA-256 of the non-MCP tool catalog, including descriptions and schemas.
    pub tool_catalog_digest: String,
    /// Selected skills in catalog order.
    pub skills: Vec<ContextSkillIdentity>,
    /// MCP configuration sources and connected tool names.
    pub mcp: ContextMcpIdentity,
    /// Effective sandbox policy label.
    pub sandbox: String,
    /// Request-affecting profile settings.
    pub profile: ContextProfileIdentity,
}
