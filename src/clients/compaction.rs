//! Bounded context-compaction preparation, summary calls, and validation.
//!
//! This module owns the seam between a conversation that is too large to send
//! and a checkpoint that replaces part of it. [`prepare`] is pure: it measures
//! the source, selects whole exchanges, applies pins, and returns a typed
//! no-op, no-fit, or prepared candidate without making a provider call.
//! [`summarize`] drives a [`SummaryCaller`] through at most four bounded
//! requests and validates every result before it can become a checkpoint.
//!
//! No production path calls this module yet: automatic compaction is task
//! 034g and manual compaction is task 034i. Keeping preparation and
//! summarization separate lets both use one selection rule and one set of
//! bounds.
#![allow(
    dead_code,
    reason = "the seam lands before its automatic and manual callers"
)]

use std::ops::Range;
use std::time::{Duration, Instant};

use anyhow::bail;
use chrono::Utc;
use uuid::Uuid;

use crate::clients::agent::{AttemptPurpose, TurnUsageSettlement};
use crate::clients::agent_runner::AgentRunner;
use crate::clients::backend::Backend;
use crate::clients::image_budget::{
    ImageBudgetError, ImageBudgetPlan, ImageOwner, ImageSlot, apply_plan_to_history,
    plan_image_budget,
};
use crate::clients::termination::{self, IntendedLimits, TurnEvidence};
use crate::clients::tools::Tool;
use crate::config::blob_store::TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES;
use crate::config::model::ResolvedModelConfig;
use crate::config::settings::ResolvedImagePolicy;
pub use crate::config::settings::{CompactionPolicy, ResolvedCompactionPolicy};
use crate::session_telemetry::{AgentRunnerTelemetryEvent, ProviderTermination};
use crate::types::compaction::{
    CompactionData, CompactionTrigger, EstimateBasis, EstimatedInput, HISTORICAL_SUMMARY_MARKER,
    MAX_CHECKPOINT_RECORD_BYTES, MessageOrigin, ModelIdentity, PinReason,
    RetainedConversationRecord, RetainedRecord, SkillRead, SummaryUsage, estimate_items_tokens,
    estimate_text_tokens,
};
use crate::types::session::{FunctionCallOutputData, MessageData, ReasoningData};
use crate::types::{ConversationItem, ImagePart, ReportedUsage, Role, Usage, UsagePresence};

/// Version of the summary instruction and transcript format below.
pub const SUMMARY_INSTRUCTION_VERSION: u32 = 1;

/// Maximum serialized bytes of one summary prompt.
pub const MAX_SUMMARY_PROMPT_BYTES: usize = 1024 * 1024;

/// Maximum logical summary requests in one compaction operation.
pub const MAX_SUMMARY_REQUESTS: u32 = 4;

/// Maximum wall time spent in summary calls, including retries.
pub const MAX_SUMMARY_TIME: Duration = Duration::from_secs(180);

/// Hard upper bound on the summary generation cap.
pub const MAX_SUMMARY_OUTPUT_TOKENS: u32 = 16_384;

/// Serialized-summary reservation kept inside the checkpoint ceiling.
pub const SUMMARY_RESERVATION_BYTES: usize = 256 * 1024;

/// The minimum reduction: `ceil(source / 10)`.
pub const MIN_REDUCTION_DIVISOR: u64 = 10;

/// A complete main-request measurement the estimator may anchor on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasuredInput {
    /// Provider-reported input tokens for a complete, positive measurement.
    pub input_tokens: u64,
    /// Number of projected items that measurement covered.
    pub covered_item_count: usize,
    /// The model the measurement belongs to.
    pub model: String,
    /// Whether the provider reported every required counter.
    pub presence: UsagePresence,
}

/// The fixed request surface that is not part of the source records.
pub struct FixedRequest<'a> {
    /// Tool definitions sent with every request.
    pub tool_schemas: &'a [Tool],
    /// The native output-schema document, when one is attached.
    pub output_schema: Option<&'a serde_json::Value>,
}

/// Whether an image-bearing source record reached an accepted main request.
///
/// Live records start [`Self::Unconsumed`] and become [`Self::Consumed`] only
/// after the main response that emitted every image is accepted. A record
/// restored without checkpoint evidence starts [`Self::Unknown`], which
/// selection treats conservatively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageConsumption {
    Consumed,
    Unconsumed,
    Unknown,
}

/// The kind of exact record an invocation may not retire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationPinKind {
    /// The real user input for the current invocation.
    UserInput,
    /// A Cake-authored correction or recovery message in this invocation.
    SyntheticInput,
    /// Text-only Developer context owned by the current task.
    DeveloperContext,
}

/// One source index whose record the current invocation protects exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvocationPin {
    pub source_index: usize,
    pub kind: InvocationPinKind,
}

/// Everything [`prepare`] needs. The source is the immutable projected request
/// the caller is about to send: fresh prompt prefix first, then the active
/// checkpoint's summary and retained tail, then subsequent raw history.
pub struct PreparationInput<'a> {
    pub source: &'a [RetainedRecord],
    /// Leading System/Developer records that are fresh prompt context.
    pub prompt_prefix_len: usize,
    /// The current task's actual user input, pinned exactly.
    pub current_user_input: Option<usize>,
    /// The task that owns this invocation.
    pub current_task_id: String,
    pub fixed: FixedRequest<'a>,
    pub context_window: Option<u32>,
    pub measured: Option<MeasuredInput>,
    /// Whether the source already contains an active checkpoint. A measured
    /// basis is stale after a checkpoint, so preparation falls back to a full
    /// estimate.
    pub has_active_checkpoint: bool,
    /// Source indices of image-bearing records whose pixels reached an
    /// accepted main response. Every other image-bearing record must carry an
    /// `unconsumed_image` pin.
    pub consumed_image_indices: &'a [usize],
    /// The request image policy used to measure the retained tail.
    pub image_policy: &'a ResolvedImagePolicy,
    pub session_id: Uuid,
    pub task_id: Uuid,
    pub model: ModelIdentity,
    pub trigger: CompactionTrigger,
    pub policy: CompactionPolicy,
}

/// The typed result of preparation.
#[derive(Debug)]
pub enum PrepareOutcome {
    /// There is nothing useful to retire.
    NoOp { reason: String },
    /// The bounds cannot be met without dropping pinned or incomplete content.
    NoFit { reason: String },
    /// A candidate ready for summary generation.
    Prepared(Box<PreparedCompaction>),
}

impl PrepareOutcome {
    /// The no-op reason, when this outcome is a no-op.
    pub fn no_op_reason(&self) -> Option<&str> {
        match self {
            Self::NoOp { reason } => Some(reason),
            Self::Prepared(_) | Self::NoFit { .. } => None,
        }
    }

    /// The no-fit reason, when this outcome is a no-fit.
    pub fn no_fit_reason(&self) -> Option<&str> {
        match self {
            Self::NoFit { reason } => Some(reason),
            Self::Prepared(_) | Self::NoOp { .. } => None,
        }
    }
}

/// A prepared candidate whose retained tail and summary bounds are fixed.
#[derive(Debug)]
pub struct PreparedCompaction {
    pub checkpoint_id: String,
    pub retained: Vec<RetainedRecord>,
    /// The read-only image plan for the retained tail.
    ///
    /// Retained records keep their original image references; this plan is
    /// used only to estimate the request projection.
    pub image_plan: ImageBudgetPlan,
    /// Retired whole exchanges, oldest first, for the summary caller.
    pub retired_exchanges: Vec<Vec<RetainedRecord>>,
    pub estimated_input: EstimatedInput,
    pub summary_tokens: u64,
    pub context_window: Option<u32>,
    pub session_id: Uuid,
    pub task_id: Uuid,
    pub model: ModelIdentity,
    pub trigger: CompactionTrigger,
}

/// One logical summary request handed to a [`SummaryCaller`].
pub struct SummaryCallRequest<'a> {
    pub step: u32,
    pub checkpoint_id: &'a str,
    pub history: &'a [ConversationItem],
    pub max_output_tokens: u32,
}

/// One provider turn returned by a [`SummaryCaller`].
#[derive(Debug)]
pub struct SummaryTurn {
    pub items: Vec<ConversationItem>,
    pub termination: Option<ProviderTermination>,
    pub usage: Option<ReportedUsage>,
    /// Every attempt that reported usage, once each, including failed and
    /// discarded attempts.
    pub attempts: Vec<ReportedUsage>,
    pub elapsed: Duration,
}

/// The summary-call seam.
///
/// The production adapter is [`AgentSummaryCaller`]; deterministic tests use a
/// fake. Implementations must send the conversation's resolved provider,
/// credentials, model, and reasoning effort with no tools, no task hooks, and
/// no output schema.
pub trait SummaryCaller {
    /// Whether the backend preserves an explicit generation cap after its
    /// final request transformation. The Codex adapter removes it.
    fn supports_generation_cap(&self) -> bool;

    /// The configured output maximum, if any.
    fn configured_output_max(&self) -> Option<u32>;

    /// Validate the summary request before any HTTP attempt.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured reasoning allowance cannot fit the
    /// summary generation cap without changing effort.
    fn validate_summary_request(&self, _generation_cap: u32) -> anyhow::Result<()> {
        Ok(())
    }

    /// Send one logical summary request.
    fn call<'a>(
        &'a mut self,
        request: SummaryCallRequest<'a>,
    ) -> impl std::future::Future<Output = anyhow::Result<SummaryTurn>> + Send + 'a;
}

/// Measure the fixed request surface (tool schemas and native output schema).
fn fixed_request_tokens(fixed: &FixedRequest<'_>) -> u64 {
    let tools = fixed
        .tool_schemas
        .iter()
        .map(|tool| serde_json::to_string(tool).unwrap_or_default())
        .fold(0_u64, |total, json| {
            total.saturating_add(estimate_text_tokens(&json))
        });
    let schema = fixed.output_schema.map_or(0, |schema| {
        let json = serde_json::to_string(schema).unwrap_or_default();
        estimate_text_tokens(&json)
    });
    tools.saturating_add(schema)
}

/// Build the provider-neutral view of the source for estimation.
fn source_items(source: &[RetainedRecord]) -> Vec<ConversationItem> {
    source
        .iter()
        .map(RetainedRecord::to_conversation_item)
        .collect()
}

/// Estimate the full projected request with the content-class estimator.
fn estimate_full_request(items: &[ConversationItem], fixed_tokens: u64) -> u64 {
    estimate_items_tokens(items).saturating_add(fixed_tokens)
}

/// Estimate opaque replay metadata carried beside the provider-visible items.
///
/// Replay declarations are not part of the provider request, but they are part
/// of the source payload and must be counted conservatively so preparation
/// does not overfill the retired slice.
fn source_metadata_tokens(records: &[RetainedRecord]) -> u64 {
    records.iter().fold(0_u64, |total, record| {
        let extra = match &record.record {
            RetainedConversationRecord::FunctionCall(data) => {
                let replay = data.replay.as_ref().map_or(0, |replay| {
                    estimate_text_tokens(&serde_json::to_string(replay).unwrap_or_default())
                });
                let parse_error = data
                    .arguments_parse_error
                    .as_deref()
                    .map_or(0, estimate_text_tokens);
                replay.saturating_add(parse_error)
            },
            RetainedConversationRecord::FunctionCallOutput(data) => {
                data.replay.as_ref().map_or(0, |replay| {
                    estimate_text_tokens(&serde_json::to_string(replay).unwrap_or_default())
                })
            },
            RetainedConversationRecord::Message(_) | RetainedConversationRecord::Reasoning(_) => 0,
        };
        total.saturating_add(extra)
    })
}

/// Estimate the post-checkpoint request: fresh prompts, summary allowance,
/// retained tail, and fixed request surface.
fn estimate_after(
    prefix: &[ConversationItem],
    retained_items: &[ConversationItem],
    summary_tokens: u64,
    fixed_tokens: u64,
) -> u64 {
    let prefix_tokens = estimate_items_tokens(prefix);
    let retained_tokens = estimate_items_tokens(retained_items);
    prefix_tokens
        .saturating_add(estimate_text_tokens(HISTORICAL_SUMMARY_MARKER))
        .saturating_add(summary_tokens)
        .saturating_add(retained_tokens)
        .saturating_add(fixed_tokens)
}

/// One complete provider exchange inside the retirable body.
#[derive(Debug, Clone)]
struct Exchange {
    range: Range<usize>,
}

impl Exchange {
    fn contains_pin(&self, source: &[RetainedRecord]) -> bool {
        source[self.range.clone()]
            .iter()
            .any(|record| record.pin.is_some())
    }
}

/// Split a retirable body into whole provider exchanges.
///
/// An exchange is one assistant response (reasoning, message, and any parallel
/// calls) plus the outputs that close those calls. User and developer messages
/// are their own exchange. Calls never share an exchange with a later assistant
/// response, so retiring an exchange cannot strand a call or output.
fn segment_exchanges(source: &[RetainedRecord], start: usize) -> anyhow::Result<Vec<Exchange>> {
    let mut exchanges = Vec::new();
    let mut index = start;
    while index < source.len() {
        let exchange_start = index;
        match &source[index].record {
            RetainedConversationRecord::Message(data)
                if matches!(data.role, Role::User | Role::Developer) =>
            {
                index += 1;
            },
            RetainedConversationRecord::Message(_)
            | RetainedConversationRecord::Reasoning(_)
            | RetainedConversationRecord::FunctionCall(_) => {
                // One assistant response: every consecutive assistant item,
                // then the outputs that close its calls.
                while index < source.len()
                    && matches!(
                        source[index].record,
                        RetainedConversationRecord::Message(_)
                            | RetainedConversationRecord::Reasoning(_)
                            | RetainedConversationRecord::FunctionCall(_)
                    )
                {
                    index += 1;
                }
                while index < source.len()
                    && matches!(
                        source[index].record,
                        RetainedConversationRecord::FunctionCallOutput(_)
                    )
                {
                    index += 1;
                }
            },
            RetainedConversationRecord::FunctionCallOutput(_) => {
                bail!(
                    "compaction source has a function call output at index {index} with no \
                     preceding function call in the same exchange"
                );
            },
        }
        exchanges.push(Exchange {
            range: exchange_start..index,
        });
    }
    Ok(exchanges)
}

/// Verify that every exchange pairs its calls and outputs completely.
fn validate_exchanges(source: &[RetainedRecord], exchanges: &[Exchange]) -> anyhow::Result<()> {
    for exchange in exchanges {
        let mut open: Vec<&str> = Vec::new();
        for record in &source[exchange.range.clone()] {
            match &record.record {
                RetainedConversationRecord::FunctionCall(data) => {
                    anyhow::ensure!(
                        !open.contains(&data.call_id.as_str()),
                        "compaction source has two unfinished function calls sharing call_id '{}'",
                        data.call_id
                    );
                    open.push(&data.call_id);
                },
                RetainedConversationRecord::FunctionCallOutput(data) => {
                    let Some(position) = open.iter().position(|call_id| *call_id == data.call_id)
                    else {
                        bail!(
                            "compaction source has an output for call_id '{}' with no \
                             preceding unmatched call",
                            data.call_id
                        );
                    };
                    open.remove(position);
                },
                RetainedConversationRecord::Message(_)
                | RetainedConversationRecord::Reasoning(_) => {
                    anyhow::ensure!(
                        open.is_empty(),
                        "compaction source has a message or reasoning item while function \
                         call '{}' still awaits its output",
                        open.first().copied().unwrap_or_default()
                    );
                },
            }
        }
        anyhow::ensure!(
            open.is_empty(),
            "compaction source ends with unfinished function call '{}'",
            open.first().copied().unwrap_or_default()
        );
    }
    Ok(())
}

/// Return the first pin or image-evidence gap that preparation must report as
/// a no-fit rather than an invented pin.
fn pin_evidence_failure(input: &PreparationInput<'_>) -> Option<String> {
    let consumed: std::collections::HashSet<usize> =
        input.consumed_image_indices.iter().copied().collect();
    for (index, record) in input.source.iter().enumerate() {
        if record.skill_read.is_some() && record.source_task.is_none() {
            return Some(format!(
                "source index {index} has ambiguous legacy skill-read provenance"
            ));
        }
        let image_count = match &record.record {
            RetainedConversationRecord::Message(data) => data.images.len(),
            RetainedConversationRecord::FunctionCallOutput(data) => data.images.len(),
            RetainedConversationRecord::Reasoning(_)
            | RetainedConversationRecord::FunctionCall(_) => 0,
        };
        if image_count > 0
            && !consumed.contains(&index)
            && record.pin != Some(PinReason::UnconsumedImage)
        {
            return Some(format!(
                "source index {index} carries {image_count} image(s) with no consumption \
                 evidence or unconsumed-image pin"
            ));
        }
    }
    None
}

/// Apply the invocation's exact pins and validate image-consumption evidence.
fn apply_pins(input: &PreparationInput<'_>) -> anyhow::Result<Vec<RetainedRecord>> {
    let mut source = input.source.to_vec();
    pin_current_user_input(&mut source, input)?;
    let consumed: std::collections::HashSet<usize> =
        input.consumed_image_indices.iter().copied().collect();
    for (index, record) in source.iter_mut().enumerate() {
        anyhow::ensure!(
            record.skill_read.is_none() || record.source_task.is_some(),
            "compaction source index {index} has skill-read provenance with no source task; \
             ambiguous legacy provenance yields no fit"
        );
        pin_skill_read(record, &input.current_task_id);
        validate_record_image_evidence(record, index, &consumed)?;
    }
    Ok(source)
}

fn pin_current_user_input(
    source: &mut [RetainedRecord],
    input: &PreparationInput<'_>,
) -> anyhow::Result<()> {
    let Some(index) = input.current_user_input else {
        return Ok(());
    };
    anyhow::ensure!(
        index < source.len(),
        "current_user_input index {index} exceeds the {} source records",
        source.len()
    );
    let record = &mut source[index];
    anyhow::ensure!(
        matches!(
            &record.record,
            RetainedConversationRecord::Message(data) if data.role == Role::User
        ),
        "current_user_input index {index} is not a user message"
    );
    record.pin = Some(PinReason::UserInput);
    record.origin = Some(MessageOrigin::User);
    Ok(())
}

fn pin_skill_read(record: &mut RetainedRecord, current_task_id: &str) {
    if record.skill_read.is_some()
        && record.source_task.as_deref() == Some(current_task_id)
        && record.pin.is_none()
        && let Some(SkillRead { name, path }) = record.skill_read.clone()
    {
        record.pin = Some(PinReason::SkillInstructions { name, path });
    }
}

/// Normalize exact pins and image evidence for one projection.
///
/// Prior-task invocation and skill pins stop applying. The current invocation's
/// explicit pins are re-applied by source index, and image evidence either adds
/// a conservative pin or clears one the accepted request satisfied.
pub(super) fn normalize_source_pins(
    records: &mut [RetainedRecord],
    current_task_id: &str,
    invocation_pins: &[InvocationPin],
    image_consumption: &[ImageConsumption],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        records.len() == image_consumption.len(),
        "compaction source has {} records but {} image-consumption states",
        records.len(),
        image_consumption.len()
    );
    for pin in invocation_pins {
        anyhow::ensure!(
            pin.source_index < records.len(),
            "compaction invocation pin source index {} exceeds the {} source records",
            pin.source_index,
            records.len()
        );
    }
    for (index, record) in records.iter_mut().enumerate() {
        anyhow::ensure!(
            record.skill_read.is_none() || record.source_task.is_some(),
            "compaction source index {index} has skill-read provenance with no source task; \
             ambiguous legacy provenance yields no fit"
        );
        clear_stale_exact_pin(record, current_task_id, index)?;
        pin_skill_read(record, current_task_id);
        for pin in invocation_pins
            .iter()
            .filter(|pin| pin.source_index == index)
        {
            apply_invocation_pin(record, index, pin.kind)?;
        }
        apply_image_consumption(record, index, image_consumption[index])?;
    }
    Ok(())
}

fn clear_stale_exact_pin(
    record: &mut RetainedRecord,
    current_task_id: &str,
    index: usize,
) -> anyhow::Result<()> {
    let Some(pin) = record.pin.as_ref() else {
        return Ok(());
    };
    if !matches!(
        pin,
        PinReason::UserInput
            | PinReason::SyntheticInput
            | PinReason::DeveloperContext
            | PinReason::SkillInstructions { .. }
    ) || record.source_task.as_deref() == Some(current_task_id)
    {
        return Ok(());
    }
    if matches!(pin, PinReason::SkillInstructions { .. }) {
        anyhow::ensure!(
            record.skill_read.is_some(),
            "compaction source index {index} has skill instructions without skill-read \
             provenance"
        );
    }
    record.pin = None;
    Ok(())
}

fn apply_invocation_pin(
    record: &mut RetainedRecord,
    index: usize,
    kind: InvocationPinKind,
) -> anyhow::Result<()> {
    match kind {
        InvocationPinKind::UserInput => {
            anyhow::ensure!(
                record_role(record) == Some(Role::User),
                "current_user_input index {index} is not a user message"
            );
            anyhow::ensure!(
                record.origin != Some(MessageOrigin::Synthetic),
                "current_user_input index {index} is synthetic input"
            );
            record.pin = Some(PinReason::UserInput);
            record.origin = Some(MessageOrigin::User);
        },
        InvocationPinKind::SyntheticInput => {
            anyhow::ensure!(
                record_role(record) == Some(Role::User),
                "synthetic_input pin at source index {index} is not a user message"
            );
            anyhow::ensure!(
                record.origin == Some(MessageOrigin::Synthetic),
                "synthetic_input pin at source index {index} has no synthetic origin"
            );
            record.pin = Some(PinReason::SyntheticInput);
        },
        InvocationPinKind::DeveloperContext => {
            anyhow::ensure!(
                record_role(record) == Some(Role::Developer),
                "developer_context pin at source index {index} is not a Developer message"
            );
            anyhow::ensure!(
                record.origin.is_none(),
                "developer_context pin at source index {index} carries a user origin"
            );
            record.pin = Some(PinReason::DeveloperContext);
        },
    }
    Ok(())
}

fn apply_image_consumption(
    record: &mut RetainedRecord,
    index: usize,
    consumption: ImageConsumption,
) -> anyhow::Result<()> {
    let images = record_images(record);
    anyhow::ensure!(
        !images.is_empty() || record.pin != Some(PinReason::UnconsumedImage),
        "compaction source index {index} has an unconsumed-image pin but carries no images"
    );
    match consumption {
        ImageConsumption::Consumed => {
            if record.pin == Some(PinReason::UnconsumedImage) {
                record.pin = None;
            }
        },
        ImageConsumption::Unconsumed | ImageConsumption::Unknown => {
            anyhow::ensure!(
                !images.is_empty(),
                "compaction source index {index} has image-consumption evidence but no images"
            );
            record.pin = Some(PinReason::UnconsumedImage);
        },
    }
    Ok(())
}

const fn record_role(record: &RetainedRecord) -> Option<Role> {
    match &record.record {
        RetainedConversationRecord::Message(data) => Some(data.role),
        RetainedConversationRecord::Reasoning(_)
        | RetainedConversationRecord::FunctionCall(_)
        | RetainedConversationRecord::FunctionCallOutput(_) => None,
    }
}

const fn record_images(record: &RetainedRecord) -> &[crate::types::ImagePart] {
    match &record.record {
        RetainedConversationRecord::Message(data) => data.images.as_slice(),
        RetainedConversationRecord::FunctionCallOutput(data) => data.images.as_slice(),
        RetainedConversationRecord::Reasoning(_) | RetainedConversationRecord::FunctionCall(_) => {
            &[]
        },
    }
}

fn validate_record_image_evidence(
    record: &RetainedRecord,
    index: usize,
    consumed: &std::collections::HashSet<usize>,
) -> anyhow::Result<()> {
    let images = record_images(record);
    for image in images {
        anyhow::ensure!(
            !image.data_base64.is_empty() || image.content_id.is_some(),
            "compaction source index {index} has an image with neither inline bytes nor a \
             content reference"
        );
    }
    if !images.is_empty()
        && !consumed.contains(&index)
        && record.pin != Some(PinReason::UnconsumedImage)
    {
        bail!(
            "compaction source index {index} carries {} image(s) with no consumption \
             evidence or unconsumed-image pin",
            images.len()
        );
    }
    anyhow::ensure!(
        record.pin != Some(PinReason::UnconsumedImage) || !images.is_empty(),
        "compaction source index {index} has an unconsumed-image pin but carries no images"
    );
    Ok(())
}

/// Simulate the externalized form used for checkpoint byte measurement.
///
/// Preparation must not write blobs, but the persisted checkpoint carries
/// digest references rather than inline image bytes or large output text. This
/// replaces exactly those payloads with a size-equivalent digest reference.
fn simulated_externalized(checkpoint: &CompactionData) -> CompactionData {
    let mut externalized = checkpoint.clone();
    for retained in &mut externalized.retained {
        match &mut retained.record {
            RetainedConversationRecord::Message(data) => {
                data.images.iter_mut().for_each(simulate_image);
            },
            RetainedConversationRecord::FunctionCallOutput(data) => {
                data.images.iter_mut().for_each(simulate_image);
                simulate_output(data);
            },
            RetainedConversationRecord::Reasoning(_)
            | RetainedConversationRecord::FunctionCall(_) => {},
        }
    }
    externalized
}

fn simulate_image(image: &mut crate::types::ImagePart) {
    const FAKE_CONTENT_ID: &str =
        "0000000000000000000000000000000000000000000000000000000000000000";
    if image.content_id.is_none() && !image.data_base64.is_empty() {
        image.content_id = Some(FAKE_CONTENT_ID.to_string());
    }
    image.data_base64.clear();
}

fn simulate_output(data: &mut crate::types::session::FunctionCallOutputData) {
    const FAKE_CONTENT_ID: &str =
        "0000000000000000000000000000000000000000000000000000000000000000";
    if data.content_id.is_none() && data.output.len() >= TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES
    {
        data.content_id = Some(FAKE_CONTENT_ID.to_string());
    }
    if data.content_id.is_some() {
        data.output.clear();
    }
}

/// The serialized size of the externalized checkpoint with a one-byte summary
/// placeholder, plus the full summary reservation.
fn checkpoint_bytes_with_reservation(checkpoint: &CompactionData) -> anyhow::Result<usize> {
    let mut measured = simulated_externalized(checkpoint);
    measured.summary = "x".to_string();
    let bytes = measured.to_record_bytes()?;
    Ok(bytes.len().saturating_add(SUMMARY_RESERVATION_BYTES))
}

/// Build the checkpoint envelope used for size measurement.
fn candidate_checkpoint(
    input: &PreparationInput<'_>,
    checkpoint_id: &str,
    retained: &[RetainedRecord],
    estimated_input: EstimatedInput,
) -> CompactionData {
    CompactionData {
        checkpoint_version: crate::types::compaction::CHECKPOINT_VERSION,
        checkpoint_id: checkpoint_id.to_string(),
        session_id: input.session_id.to_string(),
        task_id: input.task_id.to_string(),
        timestamp: Utc::now(),
        trigger: input.trigger,
        summary: String::new(),
        retained: retained.to_vec(),
        model: input.model.clone(),
        summary_instruction_version: SUMMARY_INSTRUCTION_VERSION,
        estimated_input,
        summary_usage: None,
        summary_time_ms: None,
    }
}

/// Prepare a bounded compaction candidate without making a provider call.
///
/// # Errors
///
/// Returns an error only for an invalid caller input (bad index, missing image
/// evidence, incomplete exchange pairing). Bounds that cannot be met are
/// reported as [`PrepareOutcome::NoFit`], and a source with nothing worth
/// retiring is [`PrepareOutcome::NoOp`].
#[expect(
    clippy::needless_pass_by_value,
    reason = "preparation is one decision tree over the immutable input; a borrowed input would not change behavior"
)]
pub fn prepare(input: PreparationInput<'_>) -> anyhow::Result<PrepareOutcome> {
    let resolved = input.policy.validate(input.context_window)?;
    anyhow::ensure!(
        input.prompt_prefix_len <= input.source.len(),
        "prompt_prefix_len {} exceeds the {} source records",
        input.prompt_prefix_len,
        input.source.len()
    );
    if let Some(reason) = pin_evidence_failure(&input) {
        return Ok(PrepareOutcome::NoFit { reason });
    }

    let source = apply_pins(&input)?;
    let exchanges = segment_exchanges(&source, input.prompt_prefix_len)?;
    validate_exchanges(&source, &exchanges)?;
    let items = source_items(&source);
    let fixed_tokens = fixed_request_tokens(&input.fixed);
    let (before_tokens, basis) = estimate_before(&input, &source, &items, fixed_tokens);
    if before_tokens == 0 {
        return Ok(PrepareOutcome::NoOp {
            reason: "the projected request has no measurable content".to_string(),
        });
    }

    let prefix: Vec<ConversationItem> = items[..input.prompt_prefix_len].to_vec();
    let body_exchanges: Vec<Exchange> = exchanges
        .iter()
        .filter(|exchange| exchange.range.start >= input.prompt_prefix_len)
        .cloned()
        .collect();
    if body_exchanges.is_empty() {
        return Ok(PrepareOutcome::NoOp {
            reason: "the projected request has no retirable history".to_string(),
        });
    }

    let minimum_reduction =
        before_tokens.saturating_add(MIN_REDUCTION_DIVISOR - 1) / MIN_REDUCTION_DIVISOR;
    let checkpoint_id = Uuid::new_v4().to_string();
    let selection = select_exchanges(
        &input,
        &source,
        &body_exchanges,
        &prefix,
        before_tokens,
        minimum_reduction,
        resolved,
        fixed_tokens,
        &checkpoint_id,
    )?;
    Ok(outcome_from_selection(
        selection,
        &input,
        &source,
        &body_exchanges,
        before_tokens,
        minimum_reduction,
        basis,
        checkpoint_id,
        resolved,
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the selection already carries the candidate; finishing it needs the same envelope"
)]
fn outcome_from_selection(
    selection: Selection,
    input: &PreparationInput<'_>,
    source: &[RetainedRecord],
    body_exchanges: &[Exchange],
    before_tokens: u64,
    minimum_reduction: u64,
    basis: EstimateBasis,
    checkpoint_id: String,
    resolved: ResolvedCompactionPolicy,
) -> PrepareOutcome {
    match selection {
        Selection::Fits {
            retired_ranges,
            after_tokens,
            image_plan,
        } => finalize_prepared(
            input,
            source,
            &retired_ranges,
            before_tokens,
            after_tokens,
            basis,
            checkpoint_id,
            resolved,
            image_plan,
        ),
        Selection::PinnedNoFit(reason) => PrepareOutcome::NoFit { reason },
        Selection::None => no_fit_outcome(
            source,
            body_exchanges,
            before_tokens,
            minimum_reduction,
            resolved,
        ),
    }
}

fn measured_basis_is_valid(
    input: &PreparationInput<'_>,
    measured: &MeasuredInput,
    item_count: usize,
) -> bool {
    measured.input_tokens > 0
        && measured.presence == UsagePresence::Complete
        && measured.model == input.model.model
        && !input.has_active_checkpoint
        && measured.covered_item_count >= input.prompt_prefix_len
        && measured.covered_item_count <= item_count
}

fn estimate_before(
    input: &PreparationInput<'_>,
    source: &[RetainedRecord],
    items: &[ConversationItem],
    fixed_tokens: u64,
) -> (u64, EstimateBasis) {
    let full =
        estimate_full_request(items, fixed_tokens).saturating_add(source_metadata_tokens(source));
    let Some(measured) = &input.measured else {
        return (full, EstimateBasis::ContentClass);
    };
    if !measured_basis_is_valid(input, measured, items.len()) {
        return (full, EstimateBasis::ContentClass);
    }
    let appended = estimate_items_tokens(&items[measured.covered_item_count..]).saturating_add(
        source_metadata_tokens(&source[measured.covered_item_count..]),
    );
    (
        measured.input_tokens.saturating_add(appended),
        EstimateBasis::MeasuredUsage,
    )
}

fn retired_indices(ranges: &[Range<usize>]) -> std::collections::HashSet<usize> {
    let mut retired = std::collections::HashSet::new();
    for range in ranges {
        retired.extend(range.clone());
    }
    retired
}

fn retained_source_indices(
    source_len: usize,
    prefix_len: usize,
    retired_ranges: &[Range<usize>],
) -> Vec<usize> {
    let retired = retired_indices(retired_ranges);
    (prefix_len..source_len)
        .filter(|index| !retired.contains(index))
        .collect()
}

fn retained_excluding(
    source: &[RetainedRecord],
    prefix_len: usize,
    retired_ranges: &[Range<usize>],
) -> Vec<RetainedRecord> {
    retained_source_indices(source.len(), prefix_len, retired_ranges)
        .into_iter()
        .map(|index| source[index].clone())
        .collect()
}

/// Mark every source record that image planning must treat as pinned.
///
/// A checkpoint can never retire a pinned exchange, so every image in one is
/// pinned. A record's own pin also protects it.
fn pinned_source_records(source: &[RetainedRecord], exchanges: &[Exchange]) -> Vec<bool> {
    let mut pinned: Vec<bool> = source.iter().map(|record| record.pin.is_some()).collect();
    for exchange in exchanges {
        if exchange.contains_pin(source) {
            for index in exchange.range.clone() {
                pinned[index] = true;
            }
        }
    }
    pinned
}

struct PlannedRetainedImages {
    items: Vec<ConversationItem>,
    plan: ImageBudgetPlan,
}

enum RetainedImageFailure {
    NoFit(String),
    Hard(anyhow::Error),
}

fn retained_image_slots<'a>(
    retained: &'a [RetainedRecord],
    source_indices: &[usize],
    pinned_source: &[bool],
) -> (Vec<ImageSlot<'a>>, Vec<ImageOwner>, usize) {
    let mut slots = Vec::new();
    let mut owners = Vec::new();
    let mut pinned_count = 0;
    for (item_index, (record, source_index)) in retained.iter().zip(source_indices).enumerate() {
        let pinned = pinned_source.get(*source_index).copied().unwrap_or(false);
        let images = record_images(record);
        if pinned {
            pinned_count += images.len();
        }
        for (image_index, image) in images.iter().enumerate() {
            let owner = ImageOwner {
                item: item_index,
                image: image_index,
            };
            slots.push(ImageSlot {
                ordinal: slots.len() + 1,
                owner,
                image,
                pinned,
            });
            owners.push(owner);
        }
    }
    (slots, owners, pinned_count)
}

fn pinned_image_no_fit_reason(error: &ImageBudgetError, pinned_count: usize) -> Option<String> {
    match error {
        ImageBudgetError::PinnedExceedsBudget {
            budget,
            used,
            limit,
            pinned_images,
        } => Some(format!(
            "pinned compaction images exceed the image {budget} budget: {used} used, \
             {limit} allowed across {pinned_images} pinned image(s)"
        )),
        ImageBudgetError::PinnedCannotFit {
            slot,
            media_type,
            detail,
        } => Some(format!(
            "pinned compaction image {slot} ({media_type}) cannot fit the image budget: {detail} \
             ({pinned_count} pinned image(s))"
        )),
        ImageBudgetError::InvalidBase64 { .. }
        | ImageBudgetError::DigestMismatch { .. }
        | ImageBudgetError::DimensionProbe { .. }
        | ImageBudgetError::DecompressionBomb { .. } => None,
    }
}

/// Plan the retained tail without mutating its records.
///
/// The returned items are a request-only clone with replacements and markers
/// applied for estimation. The records themselves keep their original image
/// references.
fn plan_retained_images(
    retained: &[RetainedRecord],
    source_indices: &[usize],
    pinned_source: &[bool],
    policy: &ResolvedImagePolicy,
) -> Result<PlannedRetainedImages, RetainedImageFailure> {
    let (slots, owners, pinned_count) =
        retained_image_slots(retained, source_indices, pinned_source);
    let plan = match plan_image_budget(&slots, policy) {
        Ok(plan) => plan,
        Err(error) => {
            return Err(
                pinned_image_no_fit_reason(&error, pinned_count).map_or_else(
                    || RetainedImageFailure::Hard(error.into()),
                    RetainedImageFailure::NoFit,
                ),
            );
        },
    };
    drop(slots);
    let mut items = source_items(retained);
    if let Err(error) = apply_plan_to_history(&mut items, &owners, plan.clone()) {
        return Err(RetainedImageFailure::Hard(error));
    }
    Ok(PlannedRetainedImages { items, plan })
}

fn meets_bounds(
    before_tokens: u64,
    after_tokens: u64,
    minimum_reduction: u64,
    target_tokens: Option<u64>,
) -> bool {
    let meets_target = target_tokens.is_none_or(|target| after_tokens <= target);
    let meets_reduction = before_tokens.saturating_sub(after_tokens) >= minimum_reduction;
    meets_target && meets_reduction
}

enum Selection {
    Fits {
        retired_ranges: Vec<Range<usize>>,
        after_tokens: u64,
        image_plan: ImageBudgetPlan,
    },
    PinnedNoFit(String),
    None,
}

#[expect(
    clippy::too_many_arguments,
    reason = "selection threads the source, bounds, policy, and checkpoint identity"
)]
fn select_exchanges(
    input: &PreparationInput<'_>,
    source: &[RetainedRecord],
    body_exchanges: &[Exchange],
    prefix: &[ConversationItem],
    before_tokens: u64,
    minimum_reduction: u64,
    resolved: ResolvedCompactionPolicy,
    fixed_tokens: u64,
    checkpoint_id: &str,
) -> anyhow::Result<Selection> {
    let pinned_source = pinned_source_records(source, body_exchanges);
    let mut retired_ranges: Vec<Range<usize>> = Vec::new();
    for exchange in body_exchanges {
        if exchange.contains_pin(source) {
            continue;
        }
        retired_ranges.push(exchange.range.clone());
        let retained_indices =
            retained_source_indices(source.len(), input.prompt_prefix_len, &retired_ranges);
        let retained: Vec<RetainedRecord> = retained_indices
            .iter()
            .map(|index| source[*index].clone())
            .collect();
        let planned = match plan_retained_images(
            &retained,
            &retained_indices,
            &pinned_source,
            input.image_policy,
        ) {
            Ok(planned) => planned,
            Err(RetainedImageFailure::NoFit(reason)) => {
                return Ok(Selection::PinnedNoFit(reason));
            },
            Err(RetainedImageFailure::Hard(error)) => return Err(error),
        };
        let after_tokens = estimate_after(
            prefix,
            &planned.items,
            resolved.summary_tokens,
            fixed_tokens,
        );
        if !meets_bounds(
            before_tokens,
            after_tokens,
            minimum_reduction,
            resolved.target_tokens,
        ) {
            continue;
        }
        let checkpoint = candidate_checkpoint(
            input,
            checkpoint_id,
            &retained,
            EstimatedInput {
                before_tokens,
                after_tokens,
                basis: EstimateBasis::ContentClass,
            },
        );
        if checkpoint_bytes_with_reservation(&checkpoint)
            .is_ok_and(|bytes| bytes <= MAX_CHECKPOINT_RECORD_BYTES)
        {
            return Ok(Selection::Fits {
                retired_ranges,
                after_tokens,
                image_plan: planned.plan,
            });
        }
    }
    Ok(Selection::None)
}

fn no_fit_outcome(
    source: &[RetainedRecord],
    body_exchanges: &[Exchange],
    before_tokens: u64,
    minimum_reduction: u64,
    resolved: ResolvedCompactionPolicy,
) -> PrepareOutcome {
    let all_pinned = !body_exchanges.is_empty()
        && body_exchanges
            .iter()
            .all(|exchange| exchange.contains_pin(source));
    let reason = if all_pinned {
        "every retirable exchange is pinned".to_string()
    } else {
        format!(
            "cannot reach the compaction bounds: {before_tokens} tokens before, \
             minimum reduction {minimum_reduction} tokens, target {:?}",
            resolved.target_tokens
        )
    };
    PrepareOutcome::NoFit { reason }
}

#[expect(
    clippy::too_many_arguments,
    reason = "finalization threads the selected ranges, estimates, and envelope identity"
)]
fn finalize_prepared(
    input: &PreparationInput<'_>,
    source: &[RetainedRecord],
    retired_ranges: &[Range<usize>],
    before_tokens: u64,
    after_tokens: u64,
    basis: EstimateBasis,
    checkpoint_id: String,
    resolved: ResolvedCompactionPolicy,
    image_plan: ImageBudgetPlan,
) -> PrepareOutcome {
    let retired_exchanges: Vec<Vec<RetainedRecord>> = retired_ranges
        .iter()
        .map(|range| source[range.clone()].to_vec())
        .collect();
    if retired_exchanges.is_empty() {
        return PrepareOutcome::NoOp {
            reason: "no complete unpinned exchange can be retired".to_string(),
        };
    }
    let retained = retained_excluding(source, input.prompt_prefix_len, retired_ranges);
    PrepareOutcome::Prepared(Box::new(PreparedCompaction {
        checkpoint_id,
        retained,
        image_plan,
        retired_exchanges,
        estimated_input: EstimatedInput {
            before_tokens,
            after_tokens,
            basis,
        },
        summary_tokens: resolved.summary_tokens,
        context_window: input.context_window,
        session_id: input.session_id,
        task_id: input.task_id,
        model: input.model.clone(),
        trigger: input.trigger,
    }))
}

/// The instruction prepended to every summary request.
const SUMMARY_INSTRUCTION: &str = "\
You are producing a checkpoint summary of an earlier portion of a coding session. \
The summary will replace the retired conversation in future context. Write a factual, \
self-contained handoff for another model that must continue the work. Include: the \
goal, the user's constraints and preferences, what was actually done, decisions and \
why, verification results, artifacts (paths, identifiers, commands), and the next \
steps. Use the labeled source roles. Do not invent facts, do not claim unverified \
work succeeded, and explicitly label omitted non-text content or truncated output. \
Return only the summary text.";

/// Serialize one retained record as labeled summary data.
fn format_summary_record(record: &RetainedRecord, output_limit: usize) -> String {
    match &record.record {
        RetainedConversationRecord::Message(data) => format_message(data),
        RetainedConversationRecord::FunctionCall(data) => format!(
            "[tool call {} ({})]\n{}",
            data.name, data.call_id, data.arguments
        ),
        RetainedConversationRecord::FunctionCallOutput(data) => {
            format_summary_output(data, output_limit)
        },
        RetainedConversationRecord::Reasoning(data) => format_reasoning(data),
    }
}

fn format_message(data: &MessageData) -> String {
    let mut text = if data.role == Role::User && data.content.starts_with(HISTORICAL_SUMMARY_MARKER)
    {
        format!(
            "[previous summary]\n{}",
            data.content
                .trim_start_matches(HISTORICAL_SUMMARY_MARKER)
                .trim()
        )
    } else {
        format!("[{}]\n{}", data.role.as_str(), data.content)
    };
    append_image_labels(&mut text, &data.images);
    text
}

fn format_summary_output(data: &FunctionCallOutputData, output_limit: usize) -> String {
    let output = excerpt_text(&data.output, output_limit);
    let mut text = format!("[tool result {}]\n{}", data.call_id, output);
    append_image_labels(&mut text, &data.images);
    text
}

fn append_image_labels(text: &mut String, images: &[ImagePart]) {
    for image in images {
        let identity = image.content_id.as_deref().unwrap_or("inline image");
        text.push_str("\n[image omitted: ");
        text.push_str(&image.media_type);
        text.push_str("; ");
        text.push_str(identity);
        text.push(']');
    }
}

fn format_reasoning(data: &ReasoningData) -> String {
    let mut parts = Vec::new();
    if let Some(summary) = &data.summary {
        parts.extend(summary.iter().map(|item| item.text.clone()));
    }
    if let Some(content) = &data.content {
        parts.extend(content.iter().filter_map(|item| item.text.clone()));
    }
    if !parts.is_empty() {
        return format!("[reasoning]\n{}", parts.join("\n"));
    }
    if data.encrypted_content.is_some() {
        "[reasoning omitted: encrypted]".to_string()
    } else {
        "[reasoning omitted]".to_string()
    }
}

/// Excerpt long text with a labeled head and tail.
fn excerpt_text(text: &str, limit: usize) -> String {
    if text.len() <= limit || limit == 0 {
        return text.to_string();
    }
    let head_len = limit.saturating_mul(3) / 4;
    let tail_len = limit.saturating_sub(head_len);
    let head_end = floor_char_boundary(text, head_len);
    let tail_start = ceil_char_boundary(text, text.len().saturating_sub(tail_len));
    let head = text.get(..head_end).unwrap_or_default();
    let tail = text.get(tail_start..).unwrap_or_default();
    format!(
        "{head}\n[excerpted {} bytes]\n{tail}",
        text.len()
            .saturating_sub(head_end + (text.len() - tail_start)),
    )
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// Format one retired exchange within a byte budget.
fn format_exchange(exchange: &[RetainedRecord], max_bytes: usize) -> String {
    if exchange.is_empty() {
        return String::new();
    }
    let per_record = max_bytes.saturating_div(exchange.len()).max(512);
    let mut formatted = String::new();
    for record in exchange {
        if !formatted.is_empty() {
            formatted.push_str("\n\n");
        }
        formatted.push_str(&format_summary_record(record, per_record));
        if formatted.len() > max_bytes {
            formatted.truncate(floor_char_boundary(&formatted, max_bytes));
            formatted.push_str("\n[exchange excerpted to fit the summary prompt]");
            break;
        }
    }
    formatted
}

/// Build the prompt messages for one batch.
fn summary_prompt(batch: &str, candidate: Option<&str>) -> Vec<ConversationItem> {
    let mut user = String::new();
    if let Some(candidate) = candidate {
        user.push_str("[summary so far]\n");
        user.push_str(candidate);
        user.push_str("\n\n");
    }
    user.push_str("[retired conversation]\n");
    user.push_str(batch);
    vec![
        ConversationItem::Message {
            role: Role::System,
            content: SUMMARY_INSTRUCTION.to_string(),
            id: None,
            status: None,
            timestamp: None,
            images: Vec::new(),
        },
        ConversationItem::Message {
            role: Role::User,
            content: user,
            id: None,
            status: None,
            timestamp: None,
            images: Vec::new(),
        },
    ]
}

/// The prompt size limits for one summary request.
#[derive(Debug, Clone, Copy)]
struct PromptLimits {
    bytes: usize,
    tokens: Option<u64>,
}

impl PromptLimits {
    fn from_prepared(prepared: &PreparedCompaction) -> Self {
        let tokens = prepared
            .context_window
            .map(|window| u64::from(window).saturating_mul(7) / 10)
            .filter(|limit| *limit > 0);
        Self {
            bytes: MAX_SUMMARY_PROMPT_BYTES,
            tokens,
        }
    }

    fn allows(self, prompt: &[ConversationItem]) -> bool {
        let bytes: usize = prompt
            .iter()
            .map(|item| match item {
                ConversationItem::Message { content, .. } => content.len(),
                _ => 0,
            })
            .sum();
        if bytes > self.bytes {
            return false;
        }
        self.tokens
            .is_none_or(|limit| estimate_items_tokens(prompt) <= limit)
    }
}

/// Pack retired exchanges into at most [`MAX_SUMMARY_REQUESTS`] batches.
fn pack_batches(
    prepared: &PreparedCompaction,
    limits: PromptLimits,
) -> anyhow::Result<Vec<String>> {
    let instruction_and_overhead = summary_prompt("", None)
        .iter()
        .map(|item| match item {
            ConversationItem::Message { content, .. } => content.len(),
            _ => 0,
        })
        .sum::<usize>();

    let mut batches = Vec::new();
    let mut current = String::new();

    for exchange in &prepared.retired_exchanges {
        if !current.is_empty() {
            let remaining = limits
                .bytes
                .saturating_sub(instruction_and_overhead.saturating_add(current.len()));
            let formatted = format_exchange(exchange, remaining.max(4096));
            let candidate = format!("{current}\n\n{formatted}");
            if limits.allows(&summary_prompt(&candidate, None)) {
                current = candidate;
                continue;
            }
            batches.push(std::mem::take(&mut current));
        }

        let mut budget = limits
            .bytes
            .saturating_sub(instruction_and_overhead)
            .max(4096);
        loop {
            let formatted = format_exchange(exchange, budget);
            if limits.allows(&summary_prompt(&formatted, None)) {
                current = formatted;
                break;
            }
            anyhow::ensure!(
                budget > 512,
                "one retired exchange cannot fit the summary prompt limits"
            );
            budget = budget.saturating_mul(3) / 4;
        }

        anyhow::ensure!(
            batches.len() < MAX_SUMMARY_REQUESTS as usize,
            "summary requires more than {MAX_SUMMARY_REQUESTS} logical requests"
        );
    }
    if !current.is_empty() {
        batches.push(current);
    }
    anyhow::ensure!(
        !batches.is_empty(),
        "prepared compaction has no retired content to summarize"
    );
    anyhow::ensure!(
        batches.len() <= MAX_SUMMARY_REQUESTS as usize,
        "summary requires more than {MAX_SUMMARY_REQUESTS} logical requests"
    );
    Ok(batches)
}

/// Extract the final assistant text and reject every unusable turn.
fn validated_summary_text(
    turn: &SummaryTurn,
    generation_cap: u32,
    prepared: &PreparedCompaction,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        !turn.items.iter().any(|item| matches!(
            item,
            ConversationItem::FunctionCall { .. } | ConversationItem::FunctionCallOutput { .. }
        )),
        "summary response contained tool calls"
    );
    let verdict = termination::classify(
        IntendedLimits {
            output_cap: Some(generation_cap),
            context_window: prepared.context_window,
        },
        TurnEvidence::Response {
            termination: turn.termination.as_ref(),
            usage: turn.usage,
        },
    );
    anyhow::ensure!(
        verdict.accepts_complete_output(),
        "summary response did not complete normally: {:?}",
        verdict.outcome
    );
    let text = turn
        .items
        .iter()
        .filter_map(|item| match item {
            ConversationItem::Message {
                role: Role::Assistant,
                content,
                ..
            } if !content.trim().is_empty() => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim().to_string();
    anyhow::ensure!(!text.is_empty(), "summary response contained no text");
    anyhow::ensure!(
        estimate_text_tokens(&text) <= prepared.summary_tokens,
        "summary response exceeds the {} token summary target",
        prepared.summary_tokens
    );
    Ok(text)
}

/// Add one reported usage to the checkpoint summary aggregate.
const fn add_summary_usage(total: &mut SummaryUsage, usage: Usage) {
    total.input_tokens = total.input_tokens.saturating_add(usage.input_tokens);
    total.cached_tokens = total
        .cached_tokens
        .saturating_add(usage.input_tokens_details.cached_tokens);
    total.output_tokens = total.output_tokens.saturating_add(usage.output_tokens);
    total.reasoning_tokens = total
        .reasoning_tokens
        .saturating_add(usage.output_tokens_details.reasoning_tokens);
    total.summary_visible_tokens = total.summary_visible_tokens.saturating_add(
        usage
            .output_tokens
            .saturating_sub(usage.output_tokens_details.reasoning_tokens),
    );
    total.total_tokens = total.total_tokens.saturating_add(usage.total_tokens);
}

/// Generate the summary for a prepared candidate through a bounded number of
/// logical requests.
///
/// # Errors
///
/// Returns an error when the backend does not preserve a generation cap, a
/// summary request fails, a response is unusable, the summary exceeds its
/// target, the batch count exceeds four, the deadline is exceeded, or the
/// final checkpoint cannot fit its ceiling.
pub async fn summarize<C: SummaryCaller>(
    prepared: PreparedCompaction,
    caller: &mut C,
) -> anyhow::Result<CompactionData> {
    anyhow::ensure!(
        caller.supports_generation_cap(),
        "the configured backend does not support a bounded summary generation cap"
    );
    let generation_cap = caller
        .configured_output_max()
        .unwrap_or(MAX_SUMMARY_OUTPUT_TOKENS)
        .min(MAX_SUMMARY_OUTPUT_TOKENS);
    anyhow::ensure!(
        generation_cap > 0,
        "the configured output maximum leaves no room for a summary"
    );
    caller.validate_summary_request(generation_cap)?;

    let started = Instant::now();
    let limits = PromptLimits::from_prepared(&prepared);
    let batches = pack_batches(&prepared, limits)?;
    anyhow::ensure!(
        batches.len() <= MAX_SUMMARY_REQUESTS as usize,
        "summary requires more than {MAX_SUMMARY_REQUESTS} logical requests"
    );

    let mut candidate: Option<String> = None;
    let mut usage = SummaryUsage::default();
    let mut saw_usage = false;

    for (index, batch) in batches.iter().enumerate() {
        let step = u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1);
        let prompt = summary_prompt(batch, candidate.as_deref());
        anyhow::ensure!(
            limits.allows(&prompt),
            "summary step {step} exceeds the prompt limits"
        );
        let elapsed = started.elapsed();
        anyhow::ensure!(
            elapsed < MAX_SUMMARY_TIME,
            "summary exceeded the {} second deadline before step {step}",
            MAX_SUMMARY_TIME.as_secs()
        );
        let remaining = MAX_SUMMARY_TIME.saturating_sub(elapsed);
        let turn = tokio::time::timeout(
            remaining,
            caller.call(SummaryCallRequest {
                step,
                checkpoint_id: &prepared.checkpoint_id,
                history: &prompt,
                max_output_tokens: generation_cap,
            }),
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!("summary step {step} exceeded the remaining deadline: {error}")
        })??;

        for attempt in &turn.attempts {
            saw_usage = true;
            add_summary_usage(&mut usage, attempt.usage);
        }
        candidate = Some(validated_summary_text(&turn, generation_cap, &prepared)?);
    }

    let summary = candidate.unwrap_or_default();
    anyhow::ensure!(!summary.trim().is_empty(), "summary is empty");

    let checkpoint = CompactionData {
        checkpoint_version: crate::types::compaction::CHECKPOINT_VERSION,
        checkpoint_id: prepared.checkpoint_id.clone(),
        session_id: prepared.session_id.to_string(),
        task_id: prepared.task_id.to_string(),
        timestamp: Utc::now(),
        trigger: prepared.trigger,
        summary,
        retained: prepared.retained,
        model: prepared.model,
        summary_instruction_version: SUMMARY_INSTRUCTION_VERSION,
        estimated_input: prepared.estimated_input,
        summary_usage: saw_usage.then_some(usage),
        summary_time_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
    };

    let externalized = simulated_externalized(&checkpoint);
    let bytes = externalized.to_record_bytes()?;
    anyhow::ensure!(
        bytes.len() <= MAX_CHECKPOINT_RECORD_BYTES,
        "final checkpoint is {} bytes; the limit is {MAX_CHECKPOINT_RECORD_BYTES} bytes",
        bytes.len()
    );

    // The serialized summary reservation was measured before generation; the
    // final record above is the authoritative post-generation check.
    Ok(checkpoint)
}

/// Validate that the resolved backend can serve a bounded summary request.
///
/// The check makes no network call. It rejects a backend that drops the
/// generation cap and a configured reasoning allowance that cannot fit the
/// summary cap without changing effort.
///
/// # Errors
///
/// Returns an error when the backend or reasoning allowance cannot support a
/// bounded summary.
pub fn validate_summary_backend(config: &ResolvedModelConfig) -> anyhow::Result<()> {
    let backend = Backend::from_api_type(config.model_config.api_type);
    anyhow::ensure!(
        backend_supports_generation_cap(backend, config),
        "the configured backend does not support a bounded summary generation cap"
    );
    let cap = summary_generation_cap(config);
    if let Some(reasoning) = config.model_config.reasoning_max_tokens {
        anyhow::ensure!(
            reasoning <= cap,
            "configured reasoning_max_tokens {reasoning} exceeds the summary generation \
             cap {cap}; refusing to change effort silently"
        );
    }
    Ok(())
}

/// The production summary caller through the existing provider transport.
pub struct AgentSummaryCaller<'a> {
    config: ResolvedModelConfig,
    session_id: Uuid,
    task_id: Uuid,
    telemetry: &'a mut (dyn FnMut(AgentRunnerTelemetryEvent) + Send),
    settle: &'a mut (dyn FnMut(TurnUsageSettlement) + Send),
}

impl<'a> AgentSummaryCaller<'a> {
    /// Build a production caller from the conversation's resolved config.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend drops the generation cap or the
    /// configured reasoning allowance conflicts with the summary cap.
    pub(super) fn new(
        config: &ResolvedModelConfig,
        session_id: Uuid,
        task_id: Uuid,
        telemetry: &'a mut (dyn FnMut(AgentRunnerTelemetryEvent) + Send),
        settle: &'a mut (dyn FnMut(TurnUsageSettlement) + Send),
    ) -> anyhow::Result<Self> {
        validate_summary_backend(config)?;
        Ok(Self {
            config: config.clone(),
            session_id,
            task_id,
            telemetry,
            settle,
        })
    }
}

impl SummaryCaller for AgentSummaryCaller<'_> {
    fn supports_generation_cap(&self) -> bool {
        let backend = Backend::from_api_type(self.config.model_config.api_type);
        backend_supports_generation_cap(backend, &self.config)
    }

    fn configured_output_max(&self) -> Option<u32> {
        self.config.model_config.max_output_tokens
    }

    fn validate_summary_request(&self, generation_cap: u32) -> anyhow::Result<()> {
        if let Some(reasoning) = self.config.model_config.reasoning_max_tokens {
            anyhow::ensure!(
                reasoning <= generation_cap,
                "configured reasoning_max_tokens {reasoning} exceeds the summary generation \
                 cap {generation_cap}"
            );
        }
        Ok(())
    }

    async fn call(&mut self, request: SummaryCallRequest<'_>) -> anyhow::Result<SummaryTurn> {
        let config = summary_config(&self.config, request.max_output_tokens);
        let runner = AgentRunner::new(Backend::from_api_type(config.model_config.api_type));
        let started = Instant::now();
        let purpose = AttemptPurpose::compaction(request.checkpoint_id, request.step);
        let mut next_attempt = 1;
        let mut attempts = Vec::new();
        let Self {
            session_id,
            task_id,
            telemetry,
            settle,
            ..
        } = self;
        let turn = runner
            .complete_turn(
                &config,
                *session_id,
                *task_id,
                request.step,
                request.history,
                &[],
                None,
                Some(purpose),
                &mut next_attempt,
                telemetry,
                |settlement| {
                    attempts.push(settlement.usage);
                    settle(settlement);
                },
            )
            .await?;
        Ok(SummaryTurn {
            items: turn.items,
            termination: turn.termination,
            usage: turn.usage,
            attempts,
            elapsed: started.elapsed(),
        })
    }
}

/// The resolved config for one summary request: the conversation's model,
/// provider, credentials, and effort with only the generation cap changed.
fn summary_config(base: &ResolvedModelConfig, generation_cap: u32) -> ResolvedModelConfig {
    let mut config = base.clone();
    config.model_config.max_output_tokens = Some(generation_cap);
    config
}

/// The summary generation cap for one resolved config.
pub fn summary_generation_cap(config: &ResolvedModelConfig) -> u32 {
    config
        .model_config
        .max_output_tokens
        .unwrap_or(MAX_SUMMARY_OUTPUT_TOKENS)
        .min(MAX_SUMMARY_OUTPUT_TOKENS)
}

/// Whether the backend preserves the summary generation cap.
///
/// The Codex Responses adapter removes `max_output_tokens` in its final request
/// transformation, so a bounded summary cannot be sent there. Chat Completions
/// and ordinary Responses preserves the cap.
pub(super) fn backend_supports_generation_cap(
    backend: Backend,
    config: &ResolvedModelConfig,
) -> bool {
    backend.supports_generation_cap(config)
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
