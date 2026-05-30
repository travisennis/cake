//! Usage statistics types.
//!
//! These domain types represent token usage normalized across all backends.
//! API-specific usage shapes (`ApiUsage`, `ChatUsage`) stay in their
//! respective `*_types.rs` files with their `From` impls.

use serde::{Deserialize, Serialize};

/// Usage statistics for API calls.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub input_tokens_details: InputTokensDetails,
    pub output_tokens: u64,
    pub output_tokens_details: OutputTokensDetails,
    pub total_tokens: u64,
}

/// Details about input tokens.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct InputTokensDetails {
    pub cached_tokens: u64,
}

/// Details about output tokens.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct OutputTokensDetails {
    pub reasoning_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_default_values() {
        let usage = Usage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
        assert_eq!(usage.input_tokens_details.cached_tokens, 0);
        assert_eq!(usage.output_tokens_details.reasoning_tokens, 0);
    }

    #[test]
    fn usage_serialization() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            input_tokens_details: InputTokensDetails { cached_tokens: 20 },
            output_tokens_details: OutputTokensDetails {
                reasoning_tokens: 10,
            },
        };
        let json = serde_json::to_string(&usage).unwrap();
        assert!(json.contains("\"input_tokens\":100"));
        assert!(json.contains("\"output_tokens\":50"));
        assert!(json.contains("\"total_tokens\":150"));
    }
}
