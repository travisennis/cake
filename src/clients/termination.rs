//! Classification of how one provider turn ended.
//!
//! [`classify`] is the single rule behind three questions about a turn:
//!
//! * Did a length/token-limit stop reach the intended output cap, so the model
//!   produced the output it was allowed to? Or did generation stop early, which
//!   is ambiguous rather than proof of anything?
//! * Did independent evidence establish context pressure, meaning the response
//!   cannot be used and must not be retried unchanged?
//! * Is this an unrelated failure --- a refusal, a content filter, a quota or
//!   rate-limit error, a provider failure, or an unrecognized stop --- that must
//!   never read as pressure?
//!
//! The intended cap is what the caller asked for before any context clamping
//! ([`IntendedLimits::output_cap`]), never the clamped allowance the request
//! actually carried: providers clamp silently or reject explicit caps, so the
//! value that was sent cannot be the reference.
//!
//! Only two evidence forms establish pressure: a provider's typed
//! input-overflow rejection, and a successful response whose complete,
//! provider-measured main-request input exceeds the known configured window.
//! Missing, partial, or unreported usage, estimates, a generic length stop, and
//! provider-side truncation alone never do.
//!
//! ADR 047 owns the recovery policy that consumes this verdict --- one
//! compact-and-retry per invocation, shared with an HTTP input overflow. This
//! module only classifies; it neither retries nor persists anything.

use crate::clients::retry::{self, InputOverflow};
use crate::session_telemetry::{ProviderTermination, TerminationClassification};
use crate::types::{ReportedUsage, UsagePresence};

/// The limits a request was intended to honor, before any context clamping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IntendedLimits {
    /// The intended output cap: the caller-supplied max tokens when set, else
    /// the model's configured maximum. Never the clamped value actually sent.
    pub output_cap: Option<u32>,
    /// The configured input window in tokens, when known.
    pub context_window: Option<u32>,
}

/// The one input to [`classify`], covering all three evidence forms.
///
/// Covers a failed HTTP attempt, the termination of an accepted response, and a
/// successful response's measured input and output tokens. `Copy` so a caller
/// can classify a turn without moving its usage.
#[derive(Debug, Clone, Copy)]
pub enum TurnEvidence<'a> {
    /// A non-2xx provider failure that ended the attempt.
    HttpFailure { status: u16, body: &'a str },
    /// An accepted response: its termination signal and reported usage.
    Response {
        termination: Option<&'a ProviderTermination>,
        usage: Option<ReportedUsage>,
    },
}

/// What ended the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationOutcome {
    /// The provider reported a normal completion.
    Completed,
    /// The turn ended by requesting tools.
    ToolCalls,
    /// A length/token-limit stop at or above the intended output cap: the model
    /// produced the output it was allowed to.
    OutputLimit {
        intended_cap: u32,
        output_tokens: u64,
    },
    /// A length/token-limit stop below the intended cap, or with no known cap.
    /// Ambiguous: it is neither a genuine cap stop nor proof of pressure.
    EarlyTermination {
        intended_cap: Option<u32>,
        output_tokens: Option<u64>,
    },
    /// The provider reported an incomplete response without a length stop.
    Incomplete,
    /// The provider filtered the response's content.
    ContentFilter,
    /// The provider or model refused.
    Refusal,
    /// The provider reported the response failed.
    Failed,
    /// No termination signal, or an unrecognized one.
    Unknown,
    /// The provider rejected the request because its input cannot fit alongside
    /// even the minimum output allowance.
    InputDoesNotFit(InputOverflow),
    /// The provider rejected only the requested output allowance; the input
    /// fits, so the existing allowance reduction can retry the request.
    OutputAllowanceMismatch(InputOverflow),
}

/// Independent evidence of context pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPressure {
    /// The provider typed the failure as an input overflow and the input does
    /// not fit.
    ProviderInputOverflow {
        input_tokens: u32,
        context_limit: u32,
    },
    /// A successful response reported complete main-request input usage above
    /// the configured window.
    MeasuredInput {
        input_tokens: u64,
        context_window: u32,
    },
}

/// The verdict for one provider turn.
///
/// `pressure` is the single answer to "is this context pressure"; the remaining
/// fields carry the evidence so a consumer can tell a measured comparison from
/// missing data instead of inferring it from the outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminationVerdict {
    /// What ended the turn.
    pub outcome: TerminationOutcome,
    /// The provider's own termination classification, when it reported one.
    pub provider: Option<TerminationClassification>,
    /// Context pressure established by independent evidence, if any.
    pub pressure: Option<ContextPressure>,
    /// Main-request input tokens measured by the provider, present only when
    /// the response reported complete usage.
    pub measured_input: Option<u64>,
    /// The configured input window `measured_input` was compared against.
    pub context_window: Option<u32>,
}

impl TerminationVerdict {
    /// Whether independent evidence established context pressure. A pressure
    /// verdict means the response cannot be used: it must be discarded rather
    /// than persisted, streamed, dispatched, or retried unchanged, and it never
    /// receives a semantic continuation.
    pub const fn is_context_pressure(&self) -> bool {
        self.pressure.is_some()
    }

    /// Whether the turn produced no usable final answer. Any length stop, the
    /// completion of a tool batch, a provider-reported incomplete response, a
    /// refusal, a content filter, or a failure ends the turn without an answer
    /// a caller may return.
    pub const fn marks_incomplete(&self) -> bool {
        matches!(
            self.outcome,
            TerminationOutcome::ToolCalls
                | TerminationOutcome::OutputLimit { .. }
                | TerminationOutcome::EarlyTermination { .. }
                | TerminationOutcome::Incomplete
                | TerminationOutcome::ContentFilter
                | TerminationOutcome::Refusal
                | TerminationOutcome::Failed
                | TerminationOutcome::InputDoesNotFit(_)
                | TerminationOutcome::OutputAllowanceMismatch(_)
        )
    }

    /// Whether a bounded semantic continuation may be attempted. A pressure
    /// verdict, a normal completion, and unrelated failures never get one.
    pub const fn allows_semantic_continuation(&self) -> bool {
        matches!(
            self.outcome,
            TerminationOutcome::ToolCalls
                | TerminationOutcome::OutputLimit { .. }
                | TerminationOutcome::EarlyTermination { .. }
                | TerminationOutcome::Incomplete
                | TerminationOutcome::Unknown
        )
    }

    /// Whether a caller that requires complete output --- a compaction summary
    /// caller --- may accept this turn. Every stop short of a normal completion
    /// is rejected, whether or not it is pressure: a summary must never be built
    /// from truncated, refused, or failed output (ADR 047).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the summary caller that consumes this lands with compaction (034h)"
        )
    )]
    pub const fn accepts_complete_output(&self) -> bool {
        matches!(self.outcome, TerminationOutcome::Completed)
    }

    /// The provider's own termination label, for bounded user-facing
    /// diagnostics. `None` when the provider reported no termination signal.
    pub const fn diagnostic(&self) -> Option<&'static str> {
        match self.provider {
            Some(classification) => Some(classification.as_str()),
            None => None,
        }
    }
}

/// Classify one provider turn.
///
/// `limits` supplies the intended output cap and the known input window. For an
/// [`TurnEvidence::HttpFailure`] it is unused: the failure body carries its own
/// input size and context limit.
pub fn classify(limits: IntendedLimits, evidence: TurnEvidence<'_>) -> TerminationVerdict {
    match evidence {
        TurnEvidence::HttpFailure { status, body } => classify_http_failure(status, body),
        TurnEvidence::Response { termination, usage } => {
            classify_response(limits, termination, usage)
        },
    }
}

/// Classify a provider failure.
///
/// The typed overflow form is the only failure that carries pressure evidence.
/// The retry path applies the same parse and the same
/// [`InputOverflow::fits_with_reduced_output`] rule before it reduces the output
/// allowance, so a request whose input fits cannot be read as pressure here.
fn classify_http_failure(status: u16, body: &str) -> TerminationVerdict {
    let overflow = if (400..500).contains(&status) {
        retry::parse_input_overflow(body)
    } else {
        None
    };

    let outcome = match overflow {
        Some(overflow) if overflow.fits_with_reduced_output() => {
            TerminationOutcome::OutputAllowanceMismatch(overflow)
        },
        Some(overflow) => TerminationOutcome::InputDoesNotFit(overflow),
        None => TerminationOutcome::Unknown,
    };

    let pressure = match outcome {
        TerminationOutcome::InputDoesNotFit(overflow) => {
            Some(ContextPressure::ProviderInputOverflow {
                input_tokens: overflow.input_tokens,
                context_limit: overflow.context_limit,
            })
        },
        _ => None,
    };

    TerminationVerdict {
        outcome,
        provider: None,
        pressure,
        measured_input: None,
        context_window: None,
    }
}

/// Classify an accepted response.
fn classify_response(
    limits: IntendedLimits,
    termination: Option<&ProviderTermination>,
    usage: Option<ReportedUsage>,
) -> TerminationVerdict {
    // A partial usage object does not prove which counters the provider
    // reported, so neither comparison may trust it (ADR 030).
    let complete_usage = usage
        .filter(|reported| reported.presence == UsagePresence::Complete)
        .map(|reported| reported.usage);

    let provider = termination.map(|termination| termination.classification);

    let outcome = match provider {
        None | Some(TerminationClassification::Unknown) => TerminationOutcome::Unknown,
        Some(TerminationClassification::Completed) => TerminationOutcome::Completed,
        Some(TerminationClassification::ToolCalls) => TerminationOutcome::ToolCalls,
        Some(TerminationClassification::TokenLimit) => output_limit_outcome(
            limits.output_cap,
            complete_usage.map(|usage| usage.output_tokens),
        ),
        Some(TerminationClassification::ContentFilter) => TerminationOutcome::ContentFilter,
        Some(TerminationClassification::Incomplete) => TerminationOutcome::Incomplete,
        Some(TerminationClassification::Failed) => {
            if termination.is_some_and(declares_refusal) {
                TerminationOutcome::Refusal
            } else {
                TerminationOutcome::Failed
            }
        },
    };

    let measured_input = complete_usage.map(|usage| usage.input_tokens);
    let pressure = measured_pressure(limits.context_window, measured_input, outcome);

    TerminationVerdict {
        outcome,
        provider,
        pressure,
        measured_input,
        context_window: limits.context_window,
    }
}

/// Compare a length/token-limit stop against the intended output cap.
///
/// Both reasoning and visible output count toward the cap, because the
/// provider's reported `output_tokens` already includes reasoning tokens. An
/// unknown cap or an untrusted output count is ambiguous, not a cap stop.
fn output_limit_outcome(
    intended_cap: Option<u32>,
    output_tokens: Option<u64>,
) -> TerminationOutcome {
    match (intended_cap, output_tokens) {
        (Some(intended_cap), Some(output_tokens)) if output_tokens >= u64::from(intended_cap) => {
            TerminationOutcome::OutputLimit {
                intended_cap,
                output_tokens,
            }
        },
        (intended_cap, output_tokens) => TerminationOutcome::EarlyTermination {
            intended_cap,
            output_tokens,
        },
    }
}

/// Pressure a successful response establishes by measuring its own input.
///
/// A complete measurement above the known window is pressure whatever ended the
/// turn, because the request that produced this response was already over the
/// window the caller configured, and the next request only grows. Three things
/// are never pressure: a genuine intended-cap stop, which is an output-limit
/// stop rather than a context problem; and a refusal, a content filter, or a
/// provider-reported failure, which are unrelated failures.
fn measured_pressure(
    window: Option<u32>,
    measured_input: Option<u64>,
    outcome: TerminationOutcome,
) -> Option<ContextPressure> {
    let context_window = window.filter(|window| *window > 0)?;
    let input_tokens = measured_input?;

    if input_tokens <= u64::from(context_window) {
        return None;
    }

    (!matches!(
        outcome,
        TerminationOutcome::OutputLimit { .. }
            | TerminationOutcome::ContentFilter
            | TerminationOutcome::Refusal
            | TerminationOutcome::Failed
    ))
    .then_some(ContextPressure::MeasuredInput {
        input_tokens,
        context_window,
    })
}

/// Whether the provider itself declared a refusal.
fn declares_refusal(termination: &ProviderTermination) -> bool {
    [
        termination.provider_status.as_deref(),
        termination.provider_reason.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.eq_ignore_ascii_case("refusal") || value.eq_ignore_ascii_case("refused"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Usage;

    fn termination(
        classification: TerminationClassification,
        status: Option<&str>,
        reason: Option<&str>,
    ) -> ProviderTermination {
        ProviderTermination {
            classification,
            provider_status: status.map(str::to_string),
            provider_reason: reason.map(str::to_string),
        }
    }

    fn complete_usage(input_tokens: u64, output_tokens: u64) -> ReportedUsage {
        ReportedUsage::new(
            Usage {
                input_tokens,
                output_tokens,
                total_tokens: input_tokens.saturating_add(output_tokens),
                ..Usage::default()
            },
            UsagePresence::Complete,
        )
    }

    /// Complete usage whose reported output is mostly reasoning, as providers
    /// include reasoning in `output_tokens`.
    fn reasoning_heavy_usage(
        input_tokens: u64,
        output_tokens: u64,
        reasoning_tokens: u64,
    ) -> ReportedUsage {
        ReportedUsage::new(
            Usage {
                input_tokens,
                output_tokens,
                total_tokens: input_tokens.saturating_add(output_tokens),
                output_tokens_details: crate::types::OutputTokensDetails { reasoning_tokens },
                ..Usage::default()
            },
            UsagePresence::Complete,
        )
    }

    fn partial_usage(input_tokens: u64, output_tokens: u64) -> ReportedUsage {
        ReportedUsage::new(
            Usage {
                input_tokens,
                output_tokens,
                total_tokens: input_tokens.saturating_add(output_tokens),
                ..Usage::default()
            },
            UsagePresence::Partial,
        )
    }

    fn response_verdict(
        limits: IntendedLimits,
        termination: Option<&ProviderTermination>,
        usage: Option<ReportedUsage>,
    ) -> TerminationVerdict {
        classify(limits, TurnEvidence::Response { termination, usage })
    }

    fn classify_http(status: u16, body: &str) -> TerminationVerdict {
        classify(
            IntendedLimits::default(),
            TurnEvidence::HttpFailure { status, body },
        )
    }

    const OVERFLOW_FITS: &str = r#"{"error":{"message":"input length and max_tokens exceed context limit: 12000 + 5000 > 16384"}}"#;
    const OVERFLOW_DOES_NOT_FIT: &str = r#"{"error":{"message":"input length and max_tokens exceed context limit: 16380 + 5000 > 16384"}}"#;

    /// One table row for the successful-response cases: the full contract in
    /// one place so a provider shape cannot be added without stating its
    /// classification.
    struct ResponseCase {
        name: &'static str,
        limits: IntendedLimits,
        termination: Option<ProviderTermination>,
        usage: Option<ReportedUsage>,
        expected_outcome: TerminationOutcome,
        expected_pressure: Option<ContextPressure>,
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one row per documented provider shape keeps the classification contract in one place"
    )]
    fn response_cases() -> Vec<ResponseCase> {
        let no_limits = IntendedLimits::default();
        let cap = |output_cap: u32| IntendedLimits {
            output_cap: Some(output_cap),
            context_window: None,
        };
        let window = |context_window: u32| IntendedLimits {
            output_cap: None,
            context_window: Some(context_window),
        };
        let length = || termination(TerminationClassification::TokenLimit, None, Some("length"));

        vec![
            // Clamped caps and providers that reject an explicit max-token
            // parameter: no intended cap is known, so a length stop is ambiguous.
            ResponseCase {
                name: "length stop with no intended cap",
                limits: no_limits,
                termination: Some(length()),
                usage: Some(complete_usage(10, 4096)),
                expected_outcome: TerminationOutcome::EarlyTermination {
                    intended_cap: None,
                    output_tokens: Some(4096),
                },
                expected_pressure: None,
            },
            ResponseCase {
                name: "length stop with no intended cap and no usage",
                limits: no_limits,
                termination: Some(length()),
                usage: None,
                expected_outcome: TerminationOutcome::EarlyTermination {
                    intended_cap: None,
                    output_tokens: None,
                },
                expected_pressure: None,
            },
            // Reasoning-heavy output is still output: the provider's reported
            // total, reasoning included, counts toward the cap.
            ResponseCase {
                name: "reasoning-heavy output reaches the cap",
                limits: cap(1000),
                termination: Some(length()),
                usage: Some(reasoning_heavy_usage(10, 1200, 1100)),
                expected_outcome: TerminationOutcome::OutputLimit {
                    intended_cap: 1000,
                    output_tokens: 1200,
                },
                expected_pressure: None,
            },
            // 900 visible tokens plus 100 reasoning tokens is still below a
            // 1000-token cap: the stop is ambiguous, not a cap stop.
            ResponseCase {
                name: "reasoning-heavy output below the cap",
                limits: cap(1000),
                termination: Some(length()),
                usage: Some(reasoning_heavy_usage(10, 950, 900)),
                expected_outcome: TerminationOutcome::EarlyTermination {
                    intended_cap: Some(1000),
                    output_tokens: Some(950),
                },
                expected_pressure: None,
            },
            // A genuine small cap fully used.
            ResponseCase {
                name: "small cap fully used",
                limits: cap(256),
                termination: Some(length()),
                usage: Some(complete_usage(10, 256)),
                expected_outcome: TerminationOutcome::OutputLimit {
                    intended_cap: 256,
                    output_tokens: 256,
                },
                expected_pressure: None,
            },
            // Zero output below the cap is an early stop, not a cap stop.
            ResponseCase {
                name: "zero output below the cap",
                limits: cap(1000),
                termination: Some(length()),
                usage: Some(complete_usage(10, 0)),
                expected_outcome: TerminationOutcome::EarlyTermination {
                    intended_cap: Some(1000),
                    output_tokens: Some(0),
                },
                expected_pressure: None,
            },
            ResponseCase {
                name: "output count missing below a known cap",
                limits: cap(1000),
                termination: Some(length()),
                usage: None,
                expected_outcome: TerminationOutcome::EarlyTermination {
                    intended_cap: Some(1000),
                    output_tokens: None,
                },
                expected_pressure: None,
            },
            // Complete measured input above the window, with an early stop, is
            // the one successful shape that establishes pressure.
            ResponseCase {
                name: "early length stop over the known window",
                limits: window(1000),
                termination: Some(length()),
                usage: Some(complete_usage(5000, 50)),
                expected_outcome: TerminationOutcome::EarlyTermination {
                    intended_cap: None,
                    output_tokens: Some(50),
                },
                expected_pressure: Some(ContextPressure::MeasuredInput {
                    input_tokens: 5000,
                    context_window: 1000,
                }),
            },
            ResponseCase {
                name: "incomplete response over the known window",
                limits: window(1000),
                termination: Some(termination(
                    TerminationClassification::Incomplete,
                    None,
                    None,
                )),
                usage: Some(complete_usage(5000, 50)),
                expected_outcome: TerminationOutcome::Incomplete,
                expected_pressure: Some(ContextPressure::MeasuredInput {
                    input_tokens: 5000,
                    context_window: 1000,
                }),
            },
            // Partial usage is not trustworthy, so it cannot establish pressure.
            ResponseCase {
                name: "partial usage over the known window",
                limits: window(1000),
                termination: Some(length()),
                usage: Some(partial_usage(5000, 50)),
                expected_outcome: TerminationOutcome::EarlyTermination {
                    intended_cap: None,
                    output_tokens: None,
                },
                expected_pressure: None,
            },
            // Missing usage cannot establish pressure either.
            ResponseCase {
                name: "missing usage over the known window",
                limits: window(1000),
                termination: Some(length()),
                usage: None,
                expected_outcome: TerminationOutcome::EarlyTermination {
                    intended_cap: None,
                    output_tokens: None,
                },
                expected_pressure: None,
            },
            // A measurement at or below the window is not pressure.
            ResponseCase {
                name: "measured input within the window",
                limits: window(1000),
                termination: Some(length()),
                usage: Some(complete_usage(1000, 50)),
                expected_outcome: TerminationOutcome::EarlyTermination {
                    intended_cap: None,
                    output_tokens: Some(50),
                },
                expected_pressure: None,
            },
            // No configured window: no comparison is possible.
            ResponseCase {
                name: "measured input with no known window",
                limits: no_limits,
                termination: Some(length()),
                usage: Some(complete_usage(5000, 50)),
                expected_outcome: TerminationOutcome::EarlyTermination {
                    intended_cap: None,
                    output_tokens: Some(50),
                },
                expected_pressure: None,
            },
            // Every outcome except an intended-cap stop and an unrelated
            // failure is pressure once the measurement is complete and over the
            // window: the request that produced the response was already over
            // the configured window.
            ResponseCase {
                name: "completed turn over the known window",
                limits: window(1000),
                termination: Some(termination(
                    TerminationClassification::Completed,
                    None,
                    None,
                )),
                usage: Some(complete_usage(5000, 50)),
                expected_outcome: TerminationOutcome::Completed,
                expected_pressure: Some(ContextPressure::MeasuredInput {
                    input_tokens: 5000,
                    context_window: 1000,
                }),
            },
            ResponseCase {
                name: "tool-call turn over the known window",
                limits: window(1000),
                termination: Some(termination(
                    TerminationClassification::ToolCalls,
                    None,
                    None,
                )),
                usage: Some(complete_usage(5000, 50)),
                expected_outcome: TerminationOutcome::ToolCalls,
                expected_pressure: Some(ContextPressure::MeasuredInput {
                    input_tokens: 5000,
                    context_window: 1000,
                }),
            },
            ResponseCase {
                name: "unknown termination over the known window",
                limits: window(1000),
                termination: Some(termination(
                    TerminationClassification::Unknown,
                    Some("mystery"),
                    Some("mystery"),
                )),
                usage: Some(complete_usage(5000, 50)),
                expected_outcome: TerminationOutcome::Unknown,
                expected_pressure: Some(ContextPressure::MeasuredInput {
                    input_tokens: 5000,
                    context_window: 1000,
                }),
            },
            ResponseCase {
                name: "completed turn over the known window with partial usage",
                limits: window(1000),
                termination: Some(termination(
                    TerminationClassification::Completed,
                    None,
                    None,
                )),
                usage: Some(partial_usage(5000, 50)),
                expected_outcome: TerminationOutcome::Completed,
                expected_pressure: None,
            },
            // A genuine cap stop is never pressure, whatever the input measured.
            ResponseCase {
                name: "cap stop over the known window",
                limits: IntendedLimits {
                    output_cap: Some(1000),
                    context_window: Some(1000),
                },
                termination: Some(length()),
                usage: Some(complete_usage(5000, 1000)),
                expected_outcome: TerminationOutcome::OutputLimit {
                    intended_cap: 1000,
                    output_tokens: 1000,
                },
                expected_pressure: None,
            },
            // Unrelated failures are never pressure.
            ResponseCase {
                name: "content filter over the known window",
                limits: window(1000),
                termination: Some(termination(
                    TerminationClassification::ContentFilter,
                    Some("incomplete"),
                    Some("content_filter"),
                )),
                usage: Some(complete_usage(5000, 50)),
                expected_outcome: TerminationOutcome::ContentFilter,
                expected_pressure: None,
            },
            ResponseCase {
                name: "declared refusal over the known window",
                limits: window(1000),
                termination: Some(termination(
                    TerminationClassification::Failed,
                    None,
                    Some("refusal"),
                )),
                usage: Some(complete_usage(5000, 50)),
                expected_outcome: TerminationOutcome::Refusal,
                expected_pressure: None,
            },
            ResponseCase {
                name: "failed response over the known window",
                limits: window(1000),
                termination: Some(termination(
                    TerminationClassification::Failed,
                    Some("failed"),
                    None,
                )),
                usage: Some(complete_usage(5000, 50)),
                expected_outcome: TerminationOutcome::Failed,
                expected_pressure: None,
            },
            // No termination signal is still over-window pressure.
            ResponseCase {
                name: "no termination signal over the known window",
                limits: window(1000),
                termination: None,
                usage: Some(complete_usage(5000, 50)),
                expected_outcome: TerminationOutcome::Unknown,
                expected_pressure: Some(ContextPressure::MeasuredInput {
                    input_tokens: 5000,
                    context_window: 1000,
                }),
            },
            // A zero window is not a known window.
            ResponseCase {
                name: "zero window",
                limits: window(0),
                termination: Some(length()),
                usage: Some(complete_usage(5000, 50)),
                expected_outcome: TerminationOutcome::EarlyTermination {
                    intended_cap: None,
                    output_tokens: Some(50),
                },
                expected_pressure: None,
            },
        ]
    }

    #[test]
    fn response_classification_table() {
        for case in response_cases() {
            let verdict = response_verdict(case.limits, case.termination.as_ref(), case.usage);
            assert_eq!(
                verdict.outcome, case.expected_outcome,
                "outcome for case: {}",
                case.name
            );
            assert_eq!(
                verdict.pressure, case.expected_pressure,
                "pressure for case: {}",
                case.name
            );
            assert_eq!(
                verdict.provider,
                case.termination
                    .map(|termination| termination.classification),
                "provider label for case: {}",
                case.name
            );
            assert_eq!(
                verdict.context_window, case.limits.context_window,
                "window evidence for case: {}",
                case.name
            );
        }
    }

    #[test]
    fn measured_input_evidence_distinguishes_measured_from_missing() {
        let measured = response_verdict(
            IntendedLimits {
                output_cap: None,
                context_window: Some(1000),
            },
            Some(&termination(
                TerminationClassification::TokenLimit,
                None,
                Some("length"),
            )),
            Some(complete_usage(5000, 50)),
        );
        assert_eq!(measured.measured_input, Some(5000));

        let partial = response_verdict(
            IntendedLimits {
                output_cap: None,
                context_window: Some(1000),
            },
            Some(&termination(
                TerminationClassification::TokenLimit,
                None,
                Some("length"),
            )),
            Some(partial_usage(5000, 50)),
        );
        assert_eq!(partial.measured_input, None);
        assert_eq!(partial.pressure, None);
    }

    #[test]
    fn http_overflow_with_fitting_input_is_an_allowance_mismatch() {
        let verdict = classify_http(400, OVERFLOW_FITS);

        assert!(matches!(
            verdict.outcome,
            TerminationOutcome::OutputAllowanceMismatch(overflow)
                if overflow.available_output() == 3360
        ));
        assert_eq!(verdict.pressure, None);
        assert!(!verdict.is_context_pressure());
        assert!(!verdict.allows_semantic_continuation());
    }

    #[test]
    fn http_overflow_with_input_that_does_not_fit_is_pressure() {
        let verdict = classify_http(400, OVERFLOW_DOES_NOT_FIT);

        assert!(matches!(
            verdict.outcome,
            TerminationOutcome::InputDoesNotFit(_)
        ));
        assert_eq!(
            verdict.pressure,
            Some(ContextPressure::ProviderInputOverflow {
                input_tokens: 16380,
                context_limit: 16384,
            })
        );
        assert!(verdict.is_context_pressure());
        assert!(!verdict.allows_semantic_continuation());
        assert!(verdict.marks_incomplete());
    }

    #[test]
    fn unrelated_http_failures_are_not_pressure() {
        let cases = [
            (429, r#"{"error":{"message":"rate limit exceeded"}}"#),
            (500, r#"{"error":{"message":"provider overloaded"}}"#),
            (
                400,
                r#"{"error":{"message":"input length and max_tokens exceed some other limit"}}"#,
            ),
            (400, ""),
            (200, OVERFLOW_DOES_NOT_FIT),
            (503, OVERFLOW_DOES_NOT_FIT),
        ];

        for (status, body) in cases {
            let verdict = classify_http(status, body);
            assert_eq!(
                verdict.pressure, None,
                "status {status} body {body} must not be pressure"
            );
            assert!(!verdict.is_context_pressure());
        }
    }

    #[test]
    fn quota_failure_is_not_pressure_even_with_overflow_numbers() {
        let verdict = classify_http(
            429,
            r#"{"error":{"message":"quota exceeded: context limit: 16380 + 5000 > 16384"}}"#,
        );

        assert_eq!(verdict.outcome, TerminationOutcome::Unknown);
        assert_eq!(verdict.pressure, None);
    }

    #[test]
    fn genuine_cap_stops_and_ambiguous_stops_allow_semantic_continuation() {
        let cap_stop = response_verdict(
            IntendedLimits {
                output_cap: Some(1000),
                context_window: None,
            },
            Some(&termination(
                TerminationClassification::TokenLimit,
                None,
                Some("length"),
            )),
            Some(complete_usage(10, 1000)),
        );
        assert!(!cap_stop.is_context_pressure());
        assert!(cap_stop.marks_incomplete());
        assert!(cap_stop.allows_semantic_continuation());

        let early = response_verdict(
            IntendedLimits::default(),
            Some(&termination(
                TerminationClassification::TokenLimit,
                None,
                Some("length"),
            )),
            Some(complete_usage(10, 5)),
        );
        assert!(!early.is_context_pressure());
        assert!(early.allows_semantic_continuation());
    }

    #[test]
    fn unrelated_failures_never_allow_semantic_continuation() {
        let failed = response_verdict(
            IntendedLimits::default(),
            Some(&termination(TerminationClassification::Failed, None, None)),
            None,
        );
        assert!(failed.marks_incomplete());
        assert!(!failed.allows_semantic_continuation());

        let filtered = response_verdict(
            IntendedLimits::default(),
            Some(&termination(
                TerminationClassification::ContentFilter,
                None,
                Some("content_filter"),
            )),
            None,
        );
        assert!(filtered.marks_incomplete());
        assert!(!filtered.allows_semantic_continuation());
    }

    #[test]
    fn only_a_normal_completion_is_accepted_as_summary_output() {
        let completed = response_verdict(
            IntendedLimits::default(),
            Some(&termination(
                TerminationClassification::Completed,
                None,
                None,
            )),
            Some(complete_usage(10, 5)),
        );
        assert!(completed.accepts_complete_output());

        for termination in [
            termination(TerminationClassification::TokenLimit, None, Some("length")),
            termination(TerminationClassification::Incomplete, None, None),
            termination(TerminationClassification::ContentFilter, None, None),
            termination(TerminationClassification::Failed, None, None),
            termination(TerminationClassification::ToolCalls, None, None),
            termination(TerminationClassification::Unknown, None, Some("mystery")),
        ] {
            let verdict = response_verdict(IntendedLimits::default(), Some(&termination), None);
            assert!(
                !verdict.accepts_complete_output(),
                "{termination:?} must not be accepted as summary output"
            );
        }
    }

    #[test]
    fn diagnostic_reports_the_provider_label_only() {
        let length = termination(TerminationClassification::TokenLimit, None, Some("length"));
        let verdict = response_verdict(IntendedLimits::default(), Some(&length), None);
        assert_eq!(verdict.diagnostic(), Some("token_limit"));

        let none = response_verdict(IntendedLimits::default(), None, None);
        assert_eq!(none.diagnostic(), None);
    }
}
