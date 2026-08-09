//! Client implementations for AI model providers.
//!
//! This module contains the [`Agent`] orchestrator, tool definitions, and
//! API-specific request/response handling for interacting with AI backends.
//!
//! # Architecture
//!
//! - [`Agent`] - Main orchestrator that manages conversation loops and tool execution
//! - `tools` - Tool definitions for Bash, Read, Edit, and Write operations
//! - `chat_completions` / `responses` - API-specific request handlers

mod agent;
mod agent_observer;
mod agent_runner;
mod agent_state;
mod backend;
mod chat_completions;
mod chat_types;
pub mod judge;
pub mod judge_rubric;
mod provider_strategy;
mod responses;
mod responses_types;
pub mod retry;
mod tools;

pub use agent::Agent;
#[doc(inline)]
pub use tools::ToolContext;
pub use tools::format_tool_list_section;
pub use tools::{SandboxPolicy, resolve_linked_worktree_dirs, resolve_sandbox_policy};
