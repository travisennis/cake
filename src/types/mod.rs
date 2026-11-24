//! Shared domain vocabulary used across the agent loop, backends, and
//! session persistence.
//!
//! Types in this module are backend-agnostic. API-specific request and
//! response DTOs live alongside the backend that owns them
//! (`crate::clients::chat_types`, `crate::clients::responses_types`).

pub mod compaction;
pub mod context_identity;
mod conversation;
pub mod provider_identity;
pub mod session;
mod usage;

#[doc(inline)]
pub use compaction::{CompactionData, ContextProvenance, ContextSeed, project_context};
#[doc(inline)]
pub use context_identity::{
    ContextFileIdentity, ContextIdentity, ContextMcpIdentity, ContextMcpServerIdentity,
    ContextProfileIdentity, ContextSkillIdentity, ContextSystemPromptIdentity,
};
#[doc(inline)]
pub use conversation::{
    ConversationItem, ImagePart, ReasoningContent, ReasoningContentKind, ReasoningSummary, Role,
};
#[doc(inline)]
pub use provider_identity::ProviderIdentity;
#[doc(inline)]
pub use session::{
    ApiAttemptTerminalClass, CutOffError, GitState, HookEventData, LimitExceededError,
    PromptPrefixData, PromptPrefixDecision, ReplayErrorKind, ReplaySafety, SessionRecord,
    SessionRunConfig, StoredPromptPrefix, StreamRecord, TaskCompleteData, TaskOutcome,
    TaskStartData, TurnUsageData, TurnUsagePurpose, prompt_prefix_hash,
};
#[doc(inline)]
pub use usage::{InputTokensDetails, OutputTokensDetails, ReportedUsage, Usage, UsagePresence};
