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
//! The agent loop owns the recovery policy that consumes this verdict --- one
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
    /// A Responses `response.failed` event carried a provider code that proves
    /// the request did not fit the model's context length.
    ProviderContextFailure { code: &'static str },
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
    /// verdict, a normal completion, and unrelated failures never get one; the
    /// pressure check is explicit so a future caller cannot reach a
    /// continuation through an outcome match alone.
    pub const fn allows_semantic_continuation(&self) -> bool {
        !self.is_context_pressure()
            && matches!(
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
    /// from truncated, refused, or failed output.
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
/// The retry path applies the same status gate
/// ([`retry::is_input_overflow_status`]), the same parse, and the same
/// [`InputOverflow::fits_with_reduced_output`] rule before it reduces the output
/// allowance, so a request whose input fits, and any quota, auth, or rate-limit
/// failure, cannot be read as pressure here.
fn classify_http_failure(status: u16, body: &str) -> TerminationVerdict {
    let overflow = if retry::is_input_overflow_status(status) {
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
    // reported, so neither comparison may trust it.
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
