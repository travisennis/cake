//! Shared domain vocabulary used across the agent loop, backends, and
//! session persistence.
//!
//! Types in this module are backend-agnostic. API-specific request and
//! response DTOs live alongside the backend that owns them
//! (`crate::clients::chat_types`, `crate::clients::responses_types`).

mod conversation;
pub mod session;
mod usage;

#[doc(inline)]
pub use conversation::{ConversationItem, ReasoningContent, ReasoningContentKind, Role};
#[doc(inline)]
pub use session::{
    GitState, HookEventData, SessionRecord, StreamRecord, TaskCompleteData, TaskOutcome,
    TaskStartData,
};
#[doc(inline)]
pub use usage::{InputTokensDetails, OutputTokensDetails, Usage};
