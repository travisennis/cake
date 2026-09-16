//! Usage statistics types.
//!
//! These domain types represent token usage normalized across all backends.
//! API-specific usage shapes (`ApiUsage`, `ChatUsage`) stay in their
//! respective `*_types.rs` files with their `From` impls.

use serde::{Deserialize, Serialize};

/// Whether a provider response reported the required token counters.
///
/// A provider may omit the usage object entirely, or send one with some
/// required counters absent. Both cases read as zero in [`Usage`], so this
/// marker is what keeps an unknown value from reading as a measured zero
/// (ADR-030).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UsagePresence {
    /// The response carried no usage object, or the attempt failed before one
    /// could be reported.
    #[default]
    Unreported,
    /// The response carried a usage object with at least one required counter
    /// absent. Absent counters read as zero in [`Usage`] and are not known.
    Partial,
    /// The response carried a usage object with every required counter present.
    Complete,
}

impl UsagePresence {
    /// Presence of a provider usage object whose required counters are each
    /// optional, given in wire order: input, output, and total tokens.
    ///
    /// A caller that found no usage object at all reports [`Self::Unreported`]
    /// instead of calling this.
    pub const fn from_required_counters(
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        total_tokens: Option<u64>,
    ) -> Self {
        if input_tokens.is_some() && output_tokens.is_some() && total_tokens.is_some() {
            Self::Complete
        } else {
            Self::Partial
        }
    }
}

/// Provider-reported usage and whether every required counter was reported.
///
/// Carried as one value so usage and its presence cannot drift apart while
/// they travel from a provider parser to the attempt and turn records.
#[derive(Debug, Clone, Copy)]
pub struct ReportedUsage {
    pub usage: Usage,
    pub presence: UsagePresence,
}

impl ReportedUsage {
    pub const fn new(usage: Usage, presence: UsagePresence) -> Self {
        Self { usage, presence }
    }
}

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
    /// Tokens written to the provider's prompt cache on this request.
    ///
    /// Providers may omit this field. Missing values deserialize as zero.
    #[serde(default)]
    pub cache_write_tokens: u64,
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
    fn usage_deserialization_defaults_cache_write_tokens() {
        let usage: Usage = serde_json::from_str(
            r#"{
                "input_tokens": 100,
                "input_tokens_details": {"cached_tokens": 20},
                "output_tokens": 50,
                "output_tokens_details": {"reasoning_tokens": 10},
                "total_tokens": 150
            }"#,
        )
        .unwrap();
        assert_eq!(usage.input_tokens_details.cache_write_tokens, 0);
    }

    #[test]
    fn usage_presence_serialization_is_bounded() {
        assert_eq!(
            serde_json::to_string(&UsagePresence::Unreported).unwrap(),
            "\"unreported\""
        );
        assert_eq!(
            serde_json::to_string(&UsagePresence::Partial).unwrap(),
            "\"partial\""
        );
        assert_eq!(
            serde_json::to_string(&UsagePresence::Complete).unwrap(),
            "\"complete\""
        );
        assert_eq!(UsagePresence::default(), UsagePresence::Unreported);
    }

    #[test]
    fn usage_serialization() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            input_tokens_details: InputTokensDetails {
                cached_tokens: 20,
                cache_write_tokens: 5,
            },
            output_tokens_details: OutputTokensDetails {
                reasoning_tokens: 10,
            },
        };
        let json = serde_json::to_string(&usage).unwrap();
        assert!(json.contains("\"input_tokens\":100"));
        assert!(json.contains("\"output_tokens\":50"));
        assert!(json.contains("\"total_tokens\":150"));
        assert!(json.contains("\"cache_write_tokens\":5"));
    }
}
