use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use crate::clients::compaction::{ImageConsumption, InvocationPin, InvocationPinKind};
use crate::clients::tool_output::{self, ToolOutputEntry, ToolOutputProjection};
use crate::types::compaction::prompt_context_prefix_len;
use crate::types::compaction::{
    MessageOrigin, PinReason, RetainedConversationRecord, RetainedRecord, SkillRead,
    historical_summary_record,
};
use crate::types::session::{FunctionCallData, FunctionCallOutputData, MessageData, ReasoningData};
use crate::types::{
    CompactionData, ContextProvenance, ContextSeed, ConversationItem, ImagePart, ReplaySafety,
    Role, Usage, project_context,
};

/// Checkpoint provenance for one raw-history record.
///
/// The table is parallel to [`ConversationState::history`], never becomes a
/// provider item, and is not persisted on its own. A checkpoint copies the
/// fields it needs into [`crate::types::compaction::RetainedRecord`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RecordProvenance {
    /// Who authored a user-role message; absent for every other record and for
    /// restored records whose origin was not persisted in the seed.
    pub(super) origin: Option<MessageOrigin>,
    /// The task that appended the record, when known.
    pub(super) source_task: Option<String>,
    /// A successful read of a known skill's instructions.
    pub(super) skill_read: Option<SkillRead>,
    /// The execution-time replay declaration for a function call or output.
    pub(super) replay: Option<ReplaySafety>,
    /// Whether the record's images reached an accepted main request.
    pub(super) image_consumption: ImageConsumption,
}

impl RecordProvenance {
    const fn consumed(source_task: Option<String>) -> Self {
        Self {
            origin: None,
            source_task,
            skill_read: None,
            replay: None,
            image_consumption: ImageConsumption::Consumed,
        }
    }

    /// Provenance for an item appended by the live agent loop.
    pub(super) const fn live(
        item: &ConversationItem,
        source_task: Option<String>,
        replay: Option<ReplaySafety>,
    ) -> Self {
        Self {
            origin: None,
            source_task,
            skill_read: None,
            replay,
            image_consumption: if item.image_count() == 0 {
                ImageConsumption::Consumed
            } else {
                ImageConsumption::Unconsumed
            },
        }
    }

    /// Provenance for a restored raw record. Session records carry replay
    /// declarations separately from provider items; origin, task, and skill
    /// evidence remain unknown until a caller reconstructs them.
    const fn restored(item: &ConversationItem, persisted: ContextProvenance) -> Self {
        Self {
            origin: None,
            source_task: None,
            skill_read: None,
            replay: persisted.replay,
            image_consumption: if item.image_count() == 0 {
                ImageConsumption::Consumed
            } else {
                ImageConsumption::Unknown
            },
        }
    }

    pub(super) const fn live_tool_output(
        source_task: Option<String>,
        replay: Option<ReplaySafety>,
        skill_read: Option<SkillRead>,
        has_images: bool,
    ) -> Self {
        Self {
            origin: None,
            source_task,
            skill_read,
            replay,
            image_consumption: if has_images {
                ImageConsumption::Unconsumed
            } else {
                ImageConsumption::Consumed
            },
        }
    }

    const fn with_origin(mut self, origin: MessageOrigin) -> Self {
        self.origin = Some(origin);
        self
    }
}

/// One source record in a preparation projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceRef {
    Prompt,
    Summary,
    Retained(usize),
    Raw(usize),
}

/// The exact projection checkpoint preparation may summarize and retain.
#[derive(Debug)]
pub(super) struct PreparationSource {
    pub(super) records: Vec<RetainedRecord>,
    /// Cached request summaries aligned one-to-one with `records`.
    pub(super) projections: Vec<Option<Arc<str>>>,
    pub(super) prompt_prefix_len: usize,
    pub(super) current_user_input: Option<usize>,
    pub(super) invocation_pins: Vec<InvocationPin>,
    pub(super) image_consumption: Vec<ImageConsumption>,
}

#[derive(Debug)]
pub(super) struct ConversationState {
    history: Vec<ConversationItem>,
    /// Provenance aligned one-to-one with `history` by raw index.
    provenance: Vec<RecordProvenance>,
    /// Repair outputs synthesized by [`Self::with_restored_context`] that have
    /// not yet been streamed and persisted.
    pending_repairs: Vec<ConversationItem>,
    /// The validated checkpoint that replaces retired history in requests.
    checkpoint: Option<CompactionData>,
    /// Image-consumption evidence aligned with `checkpoint.retained`.
    ///
    /// Activation seeds this from the checkpoint's pins; later accepted
    /// requests and image recovery update it without rewriting the checkpoint.
    retained_image_consumption: Vec<ImageConsumption>,
    /// Raw-history index of the active checkpoint's positional boundary:
    /// `history[..checkpoint_boundary]` is represented by `checkpoint`.
    checkpoint_boundary: usize,
    /// Cached request summaries aligned one-to-one with `history`.
    ///
    /// An entry exists only for a phase-one output that was text-only when the
    /// record was created. Later image recovery cannot make a result projected
    /// that resume would leave inline.
    projections: Vec<Option<ToolOutputProjection>>,
    /// Full-text bytes counted against the session's retained-output budget.
    /// Restored from canonical history and extended only when an oversized
    /// result is admitted at its full size.
    retained_tool_output_bytes: usize,
    /// Cached request summaries aligned with `checkpoint.retained`.
    retained_projections: Vec<Option<ToolOutputProjection>>,
}

impl ConversationState {
    pub(super) fn new(initial_messages: &[(Role, String)]) -> Self {
        let timestamp = chrono::Utc::now();
        Self {
            history: initial_messages
                .iter()
                .map(|(role, content)| ConversationItem::Message {
                    role: *role,
                    content: content.clone(),
                    id: None,
                    status: None,
                    timestamp: Some(timestamp),
                    images: Vec::new(),
                })
                .collect(),
            provenance: initial_messages
                .iter()
                .map(|_| RecordProvenance::consumed(None))
                .collect(),
            pending_repairs: Vec::new(),
            checkpoint: None,
            retained_image_consumption: Vec::new(),
            checkpoint_boundary: 0,
            projections: vec![None; initial_messages.len()],
            retained_tool_output_bytes: 0,
            retained_projections: Vec::new(),
        }
    }

    pub(super) fn history(&self) -> &[ConversationItem] {
        &self.history
    }

    /// The leading System and Developer messages that form the prompt prefix.
    pub(super) fn leading_prompt_messages(&self) -> Vec<(Role, String)> {
        self.history
            .iter()
            .take_while(|item| {
                matches!(
                    item,
                    ConversationItem::Message {
                        role: Role::System | Role::Developer,
                        ..
                    }
                )
            })
            .filter_map(|item| match item {
                ConversationItem::Message { role, content, .. } => Some((*role, content.clone())),
                _ => None,
            })
            .collect()
    }

    /// Number of leading Developer messages in the prompt prefix.
    pub(super) fn leading_developer_count(&self) -> usize {
        self.history
            .iter()
            .take_while(|item| {
                matches!(
                    item,
                    ConversationItem::Message {
                        role: Role::System | Role::Developer,
                        ..
                    }
                )
            })
            .filter(|item| {
                matches!(
                    item,
                    ConversationItem::Message {
                        role: Role::Developer,
                        ..
                    }
                )
            })
            .count()
    }

    /// Replace the leading prompt messages in place, preserving the checkpoint
    /// boundary because the caller only reuses a prefix with the same length.
    pub(super) fn replace_leading_prompt(
        &mut self,
        messages: &[(Role, String)],
    ) -> anyhow::Result<()> {
        let prompt_len = prompt_context_prefix_len(&self.history);
        anyhow::ensure!(
            prompt_len == messages.len(),
            "replacement prompt has {} messages but the current prefix has {prompt_len}",
            messages.len()
        );
        anyhow::ensure!(
            messages
                .first()
                .is_some_and(|(role, _)| *role == Role::System),
            "replacement prompt must start with a system message"
        );
        anyhow::ensure!(
            messages
                .iter()
                .all(|(role, _)| matches!(role, Role::System | Role::Developer)),
            "replacement prompt may contain only system and developer messages"
        );
        let timestamp = chrono::Utc::now();
        for (item, (role, content)) in self.history.iter_mut().zip(messages) {
            *item = ConversationItem::Message {
                role: *role,
                content: content.clone(),
                id: None,
                status: None,
                timestamp: Some(timestamp),
                images: Vec::new(),
            };
        }
        for provenance in &mut self.provenance[..prompt_len] {
            *provenance = RecordProvenance::consumed(None);
        }
        self.assert_provenance_aligned();
        Ok(())
    }

    /// The provider request derived from raw history and the active checkpoint.
    ///
    /// Raw history and its offsets stay untouched: invocation and provider-turn
    /// attribution keep indexing the raw vector, never the projection. A run
    /// without a checkpoint borrows the raw vector, so today's requests cost no
    /// copy. When `project_tool_outputs` is true, eligible phase-one results use
    /// the summary cached at append or restore; the canonical item and every
    /// persistence surface keep the full text.
    pub(super) fn request_context(
        &self,
        project_tool_outputs: bool,
    ) -> Cow<'_, [ConversationItem]> {
        if !project_tool_outputs {
            return self.base_request_context();
        }
        let source_refs = self.project_source_refs();
        if !source_refs
            .iter()
            .any(|source_ref| self.projection_for(*source_ref).is_some())
        {
            return self.base_request_context();
        }
        let base = self.base_request_context();
        let mut projected = base.into_owned();
        for (index, source_ref) in source_refs.iter().enumerate() {
            let Some(projection) = self.projection_for(*source_ref) else {
                continue;
            };
            if let Some(ConversationItem::FunctionCallOutput { output, .. }) =
                projected.get_mut(index)
            {
                *output = Arc::clone(&projection.summary);
            }
        }
        Cow::Owned(projected)
    }

    fn base_request_context(&self) -> Cow<'_, [ConversationItem]> {
        self.checkpoint.as_ref().map_or_else(
            || Cow::Borrowed(self.history.as_slice()),
            |checkpoint| {
                Cow::Owned(project_context(
                    &self.history,
                    Some(checkpoint),
                    self.checkpoint_boundary,
                ))
            },
        )
    }

    fn projection_for(&self, source_ref: SourceRef) -> Option<&ToolOutputProjection> {
        match source_ref {
            SourceRef::Prompt | SourceRef::Summary => None,
            SourceRef::Retained(index) => self
                .retained_projections
                .get(index)
                .and_then(Option::as_ref),
            SourceRef::Raw(index) => self.projections.get(index).and_then(Option::as_ref),
        }
    }

    /// The canonical oversized tool outputs in this session's retrieval index.
    pub(super) fn tool_output_entries(&self) -> Vec<ToolOutputEntry> {
        tool_output::entries_for(&self.history, &self.projections)
    }

    /// Source refs aligned with [`Self::request_context`].
    ///
    /// Projection replaces text inside an item and never changes the item
    /// count, so these refs stay aligned with the provider request.
    ///
    /// The request path uses this to map an admission plan's item index back to
    /// the raw or retained record it came from. It derives the same order as
    /// [`Self::preparation_source`] without cloning records.
    pub(super) fn request_source_refs(&self) -> Vec<SourceRef> {
        self.project_source_refs()
    }

    /// Build the exact source checkpoint preparation may summarize and retain.
    ///
    /// The records follow [`Self::request_context`]'s order: prompt prefix,
    /// active summary, retained tail, then post-boundary raw history. Source
    /// refs and image states are aligned one-to-one with those records. Pins
    /// stay as recorded; preparation applies the invocation pins after its
    /// trigger and provenance gates so legacy metadata can surface as a no-op
    /// or no-fit instead of an early error.
    pub(super) fn preparation_source(
        &self,
        current_user_raw: usize,
        invocation_start: usize,
        current_task_id: &str,
        project_tool_outputs: bool,
    ) -> anyhow::Result<PreparationSource> {
        self.ensure_current_user(current_user_raw, invocation_start)?;
        let (records, projections, source_refs, image_consumption) =
            self.project_preparation_records(project_tool_outputs)?;
        let current_user_input = self.source_index_for_user(current_user_raw, &source_refs)?;
        let invocation_pins = self.invocation_pins(
            current_user_input,
            current_user_raw,
            invocation_start,
            current_task_id,
            &source_refs,
        );
        Ok(PreparationSource {
            records,
            projections,
            prompt_prefix_len: prompt_context_prefix_len(&self.history),
            current_user_input: Some(current_user_input),
            invocation_pins,
            image_consumption,
        })
    }

    /// Replace retired history with a structurally validated checkpoint.
    ///
    /// The raw boundary moves to the current end of history. Pre-boundary raw
    /// records stay untouched as audit data but stop being projected; their
    /// protected copies are the checkpoint's retained records. The serialized
    /// ceiling belongs to the externalized write, not this hydrated copy.
    pub(super) fn activate_checkpoint(&mut self, checkpoint: CompactionData) -> anyhow::Result<()> {
        checkpoint.validate_structure()?;
        let retained_projections = self.retained_projections_for(&checkpoint);
        self.retained_image_consumption = checkpoint_image_consumption(&checkpoint);
        self.checkpoint_boundary = self.history.len();
        self.checkpoint = Some(checkpoint);
        self.retained_projections = retained_projections;
        self.assert_provenance_aligned();
        Ok(())
    }

    /// Carry one raw projection into every retained copy of the same output.
    ///
    /// A checkpoint retains canonical records, which may still carry images
    /// that an in-memory image recovery dropped. Reusing the cached raw
    /// projection keeps eligibility and preview bytes identical across the
    /// activation, a later resume, and a fork.
    fn retained_projections_for(
        &self,
        checkpoint: &CompactionData,
    ) -> Vec<Option<ToolOutputProjection>> {
        let mut raw: HashMap<(&str, Option<i64>, [u8; 32]), &ToolOutputProjection> = HashMap::new();
        for (item, projection) in self.history.iter().zip(&self.projections) {
            let ConversationItem::FunctionCallOutput {
                call_id, timestamp, ..
            } = item
            else {
                continue;
            };
            let Some(projection) = projection else {
                continue;
            };
            raw.insert(
                (
                    call_id.as_str(),
                    timestamp.as_ref().map(chrono::DateTime::timestamp_millis),
                    projection.output_digest,
                ),
                projection,
            );
        }
        checkpoint
            .retained
            .iter()
            .map(|record| {
                let RetainedConversationRecord::FunctionCallOutput(data) = &record.record else {
                    return None;
                };
                let digest = tool_output::output_digest(
                    &data.call_id,
                    data.timestamp.as_ref(),
                    &data.output,
                );
                raw.get(&(
                    data.call_id.as_str(),
                    data.timestamp
                        .as_ref()
                        .map(chrono::DateTime::timestamp_millis),
                    digest,
                ))
                .map(|projection| (*projection).clone())
            })
            .collect()
    }

    /// Mark every source record whose images reached an accepted main request.
    ///
    /// Call only after the response is accepted and only for records whose
    /// every image was emitted by the request.
    pub(super) fn mark_images_consumed(&mut self, refs: &[SourceRef]) {
        for source_ref in refs {
            match *source_ref {
                SourceRef::Prompt | SourceRef::Summary => {},
                SourceRef::Raw(raw_index) => {
                    if let Some(provenance) = self.provenance.get_mut(raw_index) {
                        provenance.image_consumption = ImageConsumption::Consumed;
                    }
                },
                SourceRef::Retained(index) => {
                    if let Some(consumption) = self.retained_image_consumption.get_mut(index) {
                        *consumption = ImageConsumption::Consumed;
                    }
                    if let Some(record) = self
                        .checkpoint
                        .as_mut()
                        .and_then(|checkpoint| checkpoint.retained.get_mut(index))
                        && record.pin == Some(PinReason::UnconsumedImage)
                    {
                        record.pin = None;
                    }
                },
            }
        }
    }

    fn ensure_current_user(
        &self,
        current_user_raw: usize,
        invocation_start: usize,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            current_user_raw < self.history.len(),
            "current user index {current_user_raw} exceeds the {} raw records",
            self.history.len()
        );
        anyhow::ensure!(
            invocation_start <= current_user_raw,
            "invocation start {invocation_start} is after current user index {current_user_raw}"
        );
        anyhow::ensure!(
            item_role(&self.history[current_user_raw]) == Some(Role::User),
            "current user index {current_user_raw} is not a user message"
        );
        anyhow::ensure!(
            self.provenance[current_user_raw].origin != Some(MessageOrigin::Synthetic),
            "current user index {current_user_raw} is synthetic input"
        );
        Ok(())
    }

    #[expect(
        clippy::type_complexity,
        reason = "the four aligned vectors are consumed together by one caller"
    )]
    fn project_preparation_records(
        &self,
        project_tool_outputs: bool,
    ) -> anyhow::Result<(
        Vec<RetainedRecord>,
        Vec<Option<Arc<str>>>,
        Vec<SourceRef>,
        Vec<ImageConsumption>,
    )> {
        let source_refs = self.project_source_refs();
        let mut records = Vec::with_capacity(source_refs.len());
        let mut projections = Vec::with_capacity(source_refs.len());
        let mut image_consumption = Vec::with_capacity(source_refs.len());
        for (source_index, source_ref) in source_refs.iter().enumerate() {
            let (record, consumption) = match *source_ref {
                SourceRef::Prompt => (
                    retained_record_from_source(
                        &self.history[source_index],
                        &self.provenance[source_index],
                    ),
                    ImageConsumption::Consumed,
                ),
                SourceRef::Summary => {
                    let checkpoint = self.checkpoint.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("summary source has no active checkpoint")
                    })?;
                    (
                        historical_summary_record(checkpoint),
                        ImageConsumption::Consumed,
                    )
                },
                SourceRef::Retained(index) => {
                    let checkpoint = self.checkpoint.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("retained source has no active checkpoint")
                    })?;
                    let record = checkpoint.retained.get(index).cloned().ok_or_else(|| {
                        anyhow::anyhow!(
                            "retained source index {index} exceeds the {} checkpoint records",
                            checkpoint.retained.len()
                        )
                    })?;
                    let consumption = self
                        .retained_image_consumption
                        .get(index)
                        .copied()
                        .unwrap_or_else(|| retained_image_consumption(&record));
                    (record, consumption)
                },
                SourceRef::Raw(raw_index) => (
                    retained_record_from_source(
                        &self.history[raw_index],
                        &self.provenance[raw_index],
                    ),
                    self.provenance[raw_index].image_consumption,
                ),
            };
            let projection = project_tool_outputs
                .then(|| self.projection_for(*source_ref))
                .flatten()
                .map(|projection| Arc::clone(&projection.summary));
            records.push(record);
            projections.push(projection);
            image_consumption.push(consumption);
        }
        Ok((records, projections, source_refs, image_consumption))
    }

    /// Projection-order source refs shared by preparation and requests.
    fn project_source_refs(&self) -> Vec<SourceRef> {
        let prompt_len = prompt_context_prefix_len(&self.history);
        let mut source_refs = vec![SourceRef::Prompt; prompt_len];
        if let Some(checkpoint) = self.checkpoint.as_ref() {
            source_refs.push(SourceRef::Summary);
            source_refs.extend((0..checkpoint.retained.len()).map(SourceRef::Retained));
        }
        let raw_start = if self.checkpoint.is_some() {
            self.checkpoint_boundary.max(prompt_len)
        } else {
            prompt_len
        };
        source_refs.extend((raw_start..self.history.len()).map(SourceRef::Raw));
        source_refs
    }

    fn source_index_for_user(
        &self,
        current_user_raw: usize,
        source_refs: &[SourceRef],
    ) -> anyhow::Result<usize> {
        if let Some(checkpoint) = self.checkpoint.as_ref()
            && current_user_raw < self.checkpoint_boundary
        {
            let retained_index = checkpoint
                .retained
                .iter()
                .rposition(|record| record.pin == Some(PinReason::UserInput))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "current user index {current_user_raw} is before the checkpoint boundary, \
                         but the checkpoint has no user_input pin"
                    )
                })?;
            anyhow::ensure!(
                retained_role(&checkpoint.retained[retained_index]) == Some(Role::User),
                "checkpoint user_input pin at retained index {retained_index} is not a user message"
            );
            return source_refs
                .iter()
                .position(|source| *source == SourceRef::Retained(retained_index))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "retained index {retained_index} is missing from the preparation source"
                    )
                });
        }
        source_refs
            .iter()
            .position(|source| *source == SourceRef::Raw(current_user_raw))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "current user index {current_user_raw} is not projected into the preparation \
                     source"
                )
            })
    }

    fn invocation_pins(
        &self,
        current_user_input: usize,
        current_user_raw: usize,
        invocation_start: usize,
        current_task_id: &str,
        source_refs: &[SourceRef],
    ) -> Vec<InvocationPin> {
        let mut pins = vec![InvocationPin {
            source_index: current_user_input,
            kind: InvocationPinKind::UserInput,
        }];
        for (source_index, source_ref) in source_refs.iter().enumerate() {
            match *source_ref {
                SourceRef::Prompt | SourceRef::Summary => {},
                SourceRef::Raw(raw_index) => self.push_raw_invocation_pins(
                    &mut pins,
                    source_index,
                    raw_index,
                    current_user_raw,
                    invocation_start,
                    current_task_id,
                ),
                SourceRef::Retained(index) => self.push_retained_developer_pin(
                    &mut pins,
                    source_index,
                    index,
                    current_task_id,
                ),
            }
        }
        pins
    }

    fn push_raw_invocation_pins(
        &self,
        pins: &mut Vec<InvocationPin>,
        source_index: usize,
        raw_index: usize,
        current_user_raw: usize,
        invocation_start: usize,
        current_task_id: &str,
    ) {
        if raw_index == current_user_raw {
            return;
        }
        let provenance = &self.provenance[raw_index];
        if provenance.origin == Some(MessageOrigin::Synthetic) && raw_index >= invocation_start {
            push_pin(pins, source_index, InvocationPinKind::SyntheticInput);
        }
        if item_role(&self.history[raw_index]) == Some(Role::Developer)
            && provenance.source_task.as_deref() == Some(current_task_id)
        {
            push_pin(pins, source_index, InvocationPinKind::DeveloperContext);
        }
    }

    fn push_retained_developer_pin(
        &self,
        pins: &mut Vec<InvocationPin>,
        source_index: usize,
        index: usize,
        current_task_id: &str,
    ) {
        let Some(record) = self
            .checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.retained.get(index))
        else {
            return;
        };
        if retained_role(record) == Some(Role::Developer)
            && record.source_task.as_deref() == Some(current_task_id)
        {
            push_pin(pins, source_index, InvocationPinKind::DeveloperContext);
        }
    }

    pub(super) fn append_developer_context(
        &mut self,
        contexts: Vec<String>,
        source_task: Option<&str>,
    ) -> Vec<ConversationItem> {
        let timestamp = chrono::Utc::now();
        let mut items = Vec::new();
        for content in contexts {
            if content.is_empty() {
                continue;
            }
            let item = ConversationItem::Message {
                role: Role::Developer,
                content,
                id: None,
                status: None,
                timestamp: Some(timestamp),
                images: Vec::new(),
            };
            self.history.push(item.clone());
            self.projections.push(None);
            self.provenance
                .push(RecordProvenance::consumed(source_task.map(str::to_owned)));
            items.push(item);
        }
        self.assert_provenance_aligned();
        items
    }

    /// Restore a persisted conversation and its latest checkpoint.
    ///
    /// The checkpoint's positional boundary is translated into raw-history
    /// coordinates after the leading prompt context is replaced, so the same
    /// records are retired on every resume and fork. Repair runs first: an
    /// unfinished call before the boundary cannot be projected, because the
    /// checkpoint would retire the call and keep its synthetic output.
    pub(super) fn with_restored_context(&mut self, seed: ContextSeed) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.history.is_empty(),
            "with_history requires Agent::new() to have set initial prompt messages"
        );
        anyhow::ensure!(
            seed.checkpoint.is_some() == seed.boundary.is_some(),
            "restored context boundary and checkpoint must be provided together"
        );
        let ContextSeed {
            messages,
            provenance,
            checkpoint,
            boundary,
        } = seed;
        anyhow::ensure!(
            provenance.is_empty() || provenance.len() == messages.len(),
            "restored context has {} provenance entries for {} history records",
            provenance.len(),
            messages.len()
        );
        let provenance = if provenance.is_empty() {
            vec![ContextProvenance::default(); messages.len()]
        } else {
            provenance
        };
        let boundary = match boundary {
            Some(boundary) => {
                anyhow::ensure!(
                    boundary <= messages.len(),
                    "restored context boundary {boundary} exceeds its {} history records",
                    messages.len()
                );
                Some(boundary)
            },
            None => None,
        };
        let prompt_len = self.history.len();
        let first_non_prompt = messages
            .iter()
            .position(|item| {
                !matches!(
                    item,
                    ConversationItem::Message {
                        role: Role::System | Role::Developer,
                        ..
                    }
                )
            })
            .unwrap_or(messages.len());
        if let Some(boundary) = boundary {
            ensure_no_open_calls(
                messages
                    .get(first_non_prompt.min(boundary)..boundary)
                    .unwrap_or_default(),
            )?;
        }
        let repairs = repair_items_for_incomplete_calls(
            messages.get(first_non_prompt..).unwrap_or_default(),
        )?;
        let restored_provenance: Vec<RecordProvenance> = messages
            .iter()
            .zip(&provenance)
            .skip(first_non_prompt)
            .map(|(item, persisted)| RecordProvenance::restored(item, *persisted))
            .collect();
        self.history
            .extend(messages.into_iter().skip(first_non_prompt));
        self.provenance.extend(restored_provenance);
        self.history.extend(repairs.iter().cloned());
        self.provenance
            .extend(repairs.iter().map(|_| RecordProvenance::consumed(None)));
        self.pending_repairs.extend(repairs);
        if let (Some(checkpoint), Some(boundary)) = (checkpoint, boundary) {
            self.retained_image_consumption = checkpoint_image_consumption(&checkpoint);
            self.checkpoint_boundary = prompt_len + boundary.saturating_sub(first_non_prompt);
            self.checkpoint = Some(checkpoint);
        }
        self.projections = tool_output::project_history(&self.history);
        self.retained_tool_output_bytes = tool_output::retained_bytes(&self.history);
        self.retained_projections = self
            .checkpoint
            .as_ref()
            .map_or_else(Vec::new, |checkpoint| {
                self.retained_projections_for(checkpoint)
            });
        self.assert_provenance_aligned();
        Ok(())
    }

    /// Take the repair outputs awaiting persistence, leaving none behind.
    pub(super) fn take_pending_repairs(&mut self) -> Vec<ConversationItem> {
        std::mem::take(&mut self.pending_repairs)
    }

    /// Append the real user input for a new invocation.
    pub(super) fn push_user_message(
        &mut self,
        content: String,
        source_task: Option<&str>,
    ) -> ConversationItem {
        self.push_user_message_with_origin(content, MessageOrigin::User, source_task)
    }

    /// Append a Cake-authored user-role message, such as a correction prompt.
    pub(super) fn push_synthetic_user_message(
        &mut self,
        content: String,
        source_task: Option<&str>,
    ) -> ConversationItem {
        self.push_user_message_with_origin(content, MessageOrigin::Synthetic, source_task)
    }

    fn push_user_message_with_origin(
        &mut self,
        content: String,
        origin: MessageOrigin,
        source_task: Option<&str>,
    ) -> ConversationItem {
        let item = ConversationItem::Message {
            role: Role::User,
            content,
            id: None,
            status: None,
            timestamp: Some(chrono::Utc::now()),
            images: Vec::new(),
        };
        self.history.push(item.clone());
        self.projections.push(None);
        self.provenance
            .push(RecordProvenance::consumed(source_task.map(str::to_owned)).with_origin(origin));
        self.assert_provenance_aligned();
        item
    }

    /// Append assistant-turn items with the exact provenance captured for each.
    ///
    /// Provider-authored items carry no cached tool-output projection; tool
    /// outputs enter through [`Self::push_tool_output`].
    pub(super) fn extend_turn_items_with_provenance(
        &mut self,
        items: Vec<ConversationItem>,
        provenance: Vec<RecordProvenance>,
    ) {
        assert_eq!(
            items.len(),
            provenance.len(),
            "every appended turn item needs one provenance entry"
        );
        self.projections
            .extend(std::iter::repeat_n(None, items.len()));
        self.history.extend(items);
        self.provenance.extend(provenance);
        self.assert_provenance_aligned();
    }

    /// Remove every inline image from the conversation, returning how many were
    /// dropped.
    ///
    /// A provider that cannot read one image rejects the whole request, and the
    /// error never identifies which image it was. Dropping all of them is the
    /// only recovery that makes the next request independent of the offending
    /// pixels; the text of each result stays, so the conversation keeps the tool
    /// outputs that carried them. The active in-memory checkpoint tail is
    /// cleared too, but the persisted checkpoint is not rewritten, so a resumed
    /// session may see the same rejected image until later recovery work.
    pub(super) fn drop_images(&mut self) -> usize {
        let dropped = self.drop_raw_images() + self.drop_retained_images();
        self.assert_provenance_aligned();
        dropped
    }

    fn drop_raw_images(&mut self) -> usize {
        let mut dropped = 0;
        for (item, provenance) in self.history.iter_mut().zip(&mut self.provenance) {
            let images = match item {
                ConversationItem::Message { images, .. }
                | ConversationItem::FunctionCallOutput { images, .. } => images,
                ConversationItem::FunctionCall { .. } | ConversationItem::Reasoning { .. } => {
                    continue;
                },
            };
            if images.is_empty() {
                continue;
            }
            dropped += images.len();
            images.clear();
            provenance.image_consumption = ImageConsumption::Consumed;
        }
        dropped
    }

    fn drop_retained_images(&mut self) -> usize {
        let Some(checkpoint) = self.checkpoint.as_mut() else {
            return 0;
        };
        let mut dropped = 0;
        for (record, consumption) in checkpoint
            .retained
            .iter_mut()
            .zip(&mut self.retained_image_consumption)
        {
            let images = match &mut record.record {
                RetainedConversationRecord::Message(data) => &mut data.images,
                RetainedConversationRecord::FunctionCallOutput(data) => &mut data.images,
                RetainedConversationRecord::Reasoning(_)
                | RetainedConversationRecord::FunctionCall(_) => continue,
            };
            if images.is_empty() {
                continue;
            }
            dropped += images.len();
            images.clear();
            *consumption = ImageConsumption::Consumed;
            if record.pin == Some(PinReason::UnconsumedImage) {
                record.pin = None;
            }
        }
        dropped
    }

    pub(super) fn push_tool_output(
        &mut self,
        tool_name: &str,
        call_id: String,
        output: String,
        images: Vec<ImagePart>,
        provenance: RecordProvenance,
    ) -> (ConversationItem, Option<ToolOutputEntry>) {
        let timestamp = chrono::Utc::now();
        let admitted =
            tool_output::admit_output(tool_name, output, self.retained_tool_output_bytes);
        self.retained_tool_output_bytes = self
            .retained_tool_output_bytes
            .saturating_add(admitted.retained_bytes);
        let output = admitted.output;
        let projection = images
            .is_empty()
            .then(|| tool_output::project_output(tool_name, &call_id, Some(&timestamp), &output))
            .flatten();
        let item = ConversationItem::FunctionCallOutput {
            call_id,
            output,
            timestamp: Some(timestamp),
            images,
        };
        let entry = projection
            .as_ref()
            .and_then(|projection| tool_output::entry_for(&item, projection));
        self.history.push(item.clone());
        self.projections.push(projection);
        self.provenance.push(provenance);
        self.assert_provenance_aligned();
        (item, entry)
    }

    /// Resolve the final assistant message among items at or after `start`.
    ///
    /// Scoping to the current turn keeps a resumed or multi-turn history's
    /// earlier assistant messages from masking a cut-off in the current turn.
    pub(super) fn resolve_assistant_message_from(&self, start: usize) -> Option<String> {
        resolve_assistant_message(self.history.get(start..).unwrap_or_default())
    }

    fn assert_provenance_aligned(&self) {
        debug_assert_eq!(
            self.provenance.len(),
            self.history.len(),
            "provenance must stay aligned with raw history"
        );
        debug_assert_eq!(
            self.projections.len(),
            self.history.len(),
            "projections must stay aligned with raw history"
        );
    }
}

fn retained_record_from_source(
    item: &ConversationItem,
    provenance: &RecordProvenance,
) -> RetainedRecord {
    let record = match item {
        ConversationItem::Message {
            role,
            content,
            id,
            status,
            timestamp,
            images,
        } => RetainedConversationRecord::Message(MessageData {
            role: *role,
            content: content.clone(),
            id: id.clone(),
            status: status.clone(),
            timestamp: *timestamp,
            images: images.clone(),
        }),
        ConversationItem::Reasoning {
            id,
            summary,
            encrypted_content,
            content,
            timestamp,
        } => RetainedConversationRecord::Reasoning(ReasoningData {
            id: id.clone(),
            summary: summary.clone(),
            encrypted_content: encrypted_content.clone(),
            content: content.clone(),
            timestamp: *timestamp,
        }),
        ConversationItem::FunctionCall {
            id,
            call_id,
            name,
            arguments,
            timestamp,
        } => RetainedConversationRecord::FunctionCall(FunctionCallData {
            id: id.clone(),
            call_id: call_id.clone(),
            name: name.clone(),
            arguments: arguments.clone(),
            arguments_parse_error: serde_json::from_str::<serde_json::Value>(arguments)
                .err()
                .map(|error| error.to_string()),
            replay: provenance.replay,
            timestamp: *timestamp,
        }),
        ConversationItem::FunctionCallOutput {
            call_id,
            output,
            timestamp,
            images,
        } => RetainedConversationRecord::FunctionCallOutput(FunctionCallOutputData {
            call_id: call_id.clone(),
            output: output.to_string(),
            content_id: None,
            replay: provenance.replay,
            timestamp: *timestamp,
            images: images.clone(),
        }),
    };
    RetainedRecord {
        record,
        pin: None,
        source_task: provenance.source_task.clone(),
        origin: provenance.origin,
        skill_read: provenance.skill_read.clone(),
    }
}

const fn item_role(item: &ConversationItem) -> Option<Role> {
    match item {
        ConversationItem::Message { role, .. } => Some(*role),
        ConversationItem::Reasoning { .. }
        | ConversationItem::FunctionCall { .. }
        | ConversationItem::FunctionCallOutput { .. } => None,
    }
}

const fn retained_role(record: &RetainedRecord) -> Option<Role> {
    match &record.record {
        RetainedConversationRecord::Message(data) => Some(data.role),
        RetainedConversationRecord::Reasoning(_)
        | RetainedConversationRecord::FunctionCall(_)
        | RetainedConversationRecord::FunctionCallOutput(_) => None,
    }
}

fn checkpoint_image_consumption(checkpoint: &CompactionData) -> Vec<ImageConsumption> {
    checkpoint
        .retained
        .iter()
        .map(retained_image_consumption)
        .collect()
}

fn retained_image_consumption(record: &RetainedRecord) -> ImageConsumption {
    if record.pin == Some(PinReason::UnconsumedImage) {
        ImageConsumption::Unconsumed
    } else {
        ImageConsumption::Consumed
    }
}

fn push_pin(pins: &mut Vec<InvocationPin>, source_index: usize, kind: InvocationPinKind) {
    let pin = InvocationPin { source_index, kind };
    if !pins.contains(&pin) {
        pins.push(pin);
    }
}

pub(super) const fn accumulate_usage(total_usage: &mut Usage, turn_usage: Option<&Usage>) {
    if let Some(usage) = turn_usage {
        total_usage.input_tokens += usage.input_tokens;
        total_usage.input_tokens_details.cached_tokens += usage.input_tokens_details.cached_tokens;
        total_usage.input_tokens_details.cache_write_tokens +=
            usage.input_tokens_details.cache_write_tokens;
        total_usage.output_tokens += usage.output_tokens;
        total_usage.output_tokens_details.reasoning_tokens +=
            usage.output_tokens_details.reasoning_tokens;
        total_usage.total_tokens += usage.total_tokens;
    }
}

/// Model-visible output for a function call the previous process abandoned.
///
/// The wording is part of what the model sees on resume; keep it stable.
fn incomplete_tool_call_repair_output(name: &str, call_id: &str) -> String {
    format!(
        "not executed: the previous cake process ended before {name}({call_id}) \
         recorded a result. Assume the tool did not run, and call it again if \
         its result is still needed."
    )
}

/// Build failed outputs for every function call left unmatched by a prior run.
///
/// Pairing walks `items` in order: a call opens, and the next output carrying
/// its `call_id` closes it. Calls still open at the end are the abandoned ones.
///
/// # Errors
///
/// Returns an error when pairing is ambiguous — two calls with the same
/// `call_id` open at once, or an output whose `call_id` has no open call —
/// rather than guessing which call an output belongs to.
fn repair_items_for_incomplete_calls(
    items: &[ConversationItem],
) -> anyhow::Result<Vec<ConversationItem>> {
    let open = open_function_calls(items)?;
    let timestamp = chrono::Utc::now();
    Ok(open
        .into_iter()
        .map(|(call_id, name)| ConversationItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: Arc::from(incomplete_tool_call_repair_output(name, call_id)),
            timestamp: Some(timestamp),
            images: Vec::new(),
        })
        .collect())
}

/// Reject history whose unfinished call is retired by a checkpoint.
///
/// The projector would drop the call but keep its repair output, leaving an
/// output with no call in the provider request.
fn ensure_no_open_calls(items: &[ConversationItem]) -> anyhow::Result<()> {
    if let Some((call_id, _)) = open_function_calls(items)?.first() {
        anyhow::bail!(
            "Corrupt session history: the checkpoint retires the unfinished function call \
             '{call_id}', so no complete exchange can be projected."
        );
    }
    Ok(())
}

/// Return every function call left open at the end of `items`, in issue order.
fn open_function_calls(items: &[ConversationItem]) -> anyhow::Result<Vec<(&str, &str)>> {
    let mut open: Vec<(&str, &str)> = Vec::new();
    for item in items {
        match item {
            ConversationItem::FunctionCall { call_id, name, .. } => {
                anyhow::ensure!(
                    !open.iter().any(|(open_id, _)| *open_id == call_id),
                    "Corrupt session history: two unfinished function calls share \
                     call_id '{call_id}'; cannot determine which call an output \
                     belongs to."
                );
                open.push((call_id, name));
            },
            ConversationItem::FunctionCallOutput { call_id, .. } => {
                let position = open
                    .iter()
                    .position(|(open_id, _)| *open_id == call_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Corrupt session history: function call output for \
                             call_id '{call_id}' has no preceding unmatched \
                             function call."
                        )
                    })?;
                open.remove(position);
            },
            ConversationItem::Message { .. } | ConversationItem::Reasoning { .. } => {},
        }
    }
    Ok(open)
}

fn resolve_assistant_message(items: &[ConversationItem]) -> Option<String> {
    items.iter().rev().find_map(|item| {
        if let ConversationItem::Message {
            role: Role::Assistant,
            content,
            ..
        } = item
            && !content.trim().is_empty()
        {
            Some(content.clone())
        } else {
            None
        }
    })
}
