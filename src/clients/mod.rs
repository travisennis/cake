//! Client implementations for AI model providers.
//!
//! This module contains the [`Agent`] orchestrator, tool definitions, and
//! API-specific request/response handling for interacting with AI backends.
//!
//! # Architecture
//!
//! - [`Agent`] - Main orchestrator that manages conversation loops and tool execution
//! - `tools` - Tool definitions for Bash, `BashSession`, Read, Edit, and Write operations
//! - `chat_completions` / `responses` - API-specific request handlers

mod agent;
mod agent_observer;
mod agent_runner;
mod agent_state;
pub mod backend;
mod chat_completions;
mod chat_types;
mod provider_strategy;
pub mod response_stall;
mod responses;
mod responses_types;
pub mod retry;
pub mod termination;
mod tools;

pub use agent::Agent;
#[doc(inline)]
pub use tools::ToolContext;
pub use tools::format_tool_list_section;
pub use tools::{SandboxPolicy, resolve_linked_worktree_dirs, resolve_sandbox_policy};

/// Re-exported so hook payloads parse tool arguments through the same repair
/// pass the corresponding tool executors apply, keeping the hook view of
/// `tool_input` aligned with what the tool will act on (#185). The crate has
/// no library target, so this is internal. Which tools repair is declared by
/// each registry entry and queried per call (#277).
pub use tools::repair_json_args;

/// Return a provider-error detail that is safe for telemetry and other
/// out-of-band sinks.
///
/// Typed provider errors keep a bounded response preview for human diagnostics.
/// These sinks must not carry provider response bytes, so this walks the error
/// chain and returns the preview-free detail for each known type.
pub fn telemetry_safe_provider_error_detail(error: &anyhow::Error) -> Option<String> {
    for cause in error.chain() {
        if let Some(stall) = cause.downcast_ref::<response_stall::ResponseStall>() {
            return Some(stall.telemetry_detail());
        }
        if let Some(decode) = cause.downcast_ref::<backend::ResponseDecodeError>() {
            return Some(decode.telemetry_detail());
        }
        if let Some(parse) = cause.downcast_ref::<backend::ResponseParseError>() {
            return Some(parse.telemetry_detail().to_string());
        }
    }
    None
}
