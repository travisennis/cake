use std::borrow::Cow;

use crate::clients::compaction::{
    ImageConsumption, InvocationPin, InvocationPinKind, normalize_source_pins,
};
use crate::types::compaction::prompt_context_prefix_len;
use crate::types::compaction::{
    MessageOrigin, PinReason, RetainedConversationRecord, RetainedRecord, SkillRead,
    historical_summary_record,
};
use crate::types::session::{FunctionCallData, FunctionCallOutputData, MessageData, ReasoningData};
use crate::types::{
    CompactionData, ContextSeed, ConversationItem, ImagePart, ReplaySafety, Role, Usage,
    project_context,
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

    /// Provenance for a restored raw record. Session records are not part of
    /// this seed, so origin, task, skill, and replay evidence stay unknown.
    const fn restored(item: &ConversationItem) -> Self {
        Self {
            origin: None,
            source_task: None,
            skill_read: None,
            replay: None,
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

/// One retirable raw-history record in source order.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "checkpoint preparation consumes this ordering in Milestone 2"
    )
)]
#[derive(Debug, Clone, Copy)]
pub(super) struct RawSourceRecord<'a> {
    pub(super) raw_index: usize,
    pub(super) item: &'a ConversationItem,
    pub(super) provenance: &'a RecordProvenance,
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
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "checkpoint preparation consumes these fields in Milestone 3"
    )
)]
#[derive(Debug)]
pub(super) struct PreparationSource {
    pub(super) records: Vec<RetainedRecord>,
    pub(super) prompt_prefix_len: usize,
    pub(super) current_user_input: Option<usize>,
    pub(super) invocation_pins: Vec<InvocationPin>,
    pub(super) image_consumption: Vec<ImageConsumption>,
    pub(super) source_refs: Vec<SourceRef>,
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
        }
    }

    pub(super) fn history(&self) -> &[ConversationItem] {
        &self.history
    }

    /// Provenance aligned one-to-one with [`Self::history`] by raw index.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "checkpoint preparation consumes the side table in Milestone 2"
        )
    )]
    pub(super) fn provenance(&self) -> &[RecordProvenance] {
        &self.provenance
    }

    /// Raw body records in retirement order: oldest first, after the fresh
    /// prompt prefix and any active checkpoint boundary.
    ///
    /// These are candidates, not final eligibility: preparation removes pinned
    /// exchanges later. Retained records in an active checkpoint are ordered by
    /// `checkpoint.retained`, not by this raw slice.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "checkpoint preparation consumes this ordering in Milestone 2"
        )
    )]
    pub(super) fn retirable_raw_source_records(&self) -> impl Iterator<Item = RawSourceRecord<'_>> {
        let start = prompt_context_prefix_len(&self.history).max(self.checkpoint_boundary);
        (start..self.history.len()).map(|raw_index| RawSourceRecord {
            raw_index,
            item: &self.history[raw_index],
            provenance: &self.provenance[raw_index],
        })
    }

    /// The provider request derived from raw history and the active checkpoint.
    ///
    /// Raw history and its offsets stay untouched: invocation and provider-turn
    /// attribution keep indexing the raw vector, never the projection. A run
    /// without a checkpoint borrows the raw vector, so today's requests cost no
    /// copy.
    pub(super) fn request_context(&self) -> Cow<'_, [ConversationItem]> {
        let Some(checkpoint) = self.checkpoint.as_ref() else {
            return Cow::Borrowed(&self.history);
        };
        Cow::Owned(project_context(
            &self.history,
            Some(checkpoint),
            self.checkpoint_boundary,
        ))
    }

    /// Whether a checkpoint currently replaces part of the request context.
    #[cfg(test)]
    pub(super) const fn has_checkpoint(&self) -> bool {
        self.checkpoint.is_some()
    }

    /// Build the exact source checkpoint preparation may summarize and retain.
    ///
    /// The records follow [`Self::request_context`]'s order: prompt prefix,
    /// active summary, retained tail, then post-boundary raw history. Source
    /// refs and image states are aligned one-to-one with those records.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "checkpoint preparation consumes this source in Milestone 3"
        )
    )]
    pub(super) fn preparation_source(
        &self,
        current_user_raw: usize,
        invocation_start: usize,
        current_task_id: &str,
    ) -> anyhow::Result<PreparationSource> {
        self.ensure_current_user(current_user_raw, invocation_start)?;
        let (mut records, source_refs, image_consumption) = self.project_preparation_records();
        let current_user_input = self.source_index_for_user(current_user_raw, &source_refs)?;
        let invocation_pins = self.invocation_pins(
            current_user_input,
            current_user_raw,
            invocation_start,
            current_task_id,
            &source_refs,
        );
        normalize_source_pins(
            &mut records,
            current_task_id,
            &invocation_pins,
            &image_consumption,
        )?;
        Ok(PreparationSource {
            records,
            prompt_prefix_len: prompt_context_prefix_len(&self.history),
            current_user_input: Some(current_user_input),
            invocation_pins,
            image_consumption,
            source_refs,
        })
    }

    /// Replace retired history with a validated checkpoint.
    ///
    /// The raw boundary moves to the current end of history. Pre-boundary raw
    /// records stay untouched as audit data but stop being projected; their
    /// protected copies are the checkpoint's retained records.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the pre-request compaction caller activates checkpoints in Milestone 4"
        )
    )]
    pub(super) fn activate_checkpoint(&mut self, checkpoint: CompactionData) -> anyhow::Result<()> {
        checkpoint.validate()?;
        self.align_provenance_for_test_history();
        self.retained_image_consumption = checkpoint_image_consumption(&checkpoint);
        self.checkpoint_boundary = self.history.len();
        self.checkpoint = Some(checkpoint);
        self.assert_provenance_aligned();
        Ok(())
    }

    /// Mark every source record whose images reached an accepted main request.
    ///
    /// Call only after the response is accepted and only for records whose
    /// every image was emitted by the request.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the accepted-response caller records consumed images in Milestone 4"
        )
    )]
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

    fn project_preparation_records(
        &self,
    ) -> (Vec<RetainedRecord>, Vec<SourceRef>, Vec<ImageConsumption>) {
        let prompt_len = prompt_context_prefix_len(&self.history);
        let mut records = Vec::new();
        let mut source_refs = Vec::new();
        let mut image_consumption = Vec::new();
        for raw_index in 0..prompt_len {
            records.push(retained_record_from_source(
                &self.history[raw_index],
                &self.provenance[raw_index],
            ));
            source_refs.push(SourceRef::Prompt);
            image_consumption.push(ImageConsumption::Consumed);
        }
        if let Some(checkpoint) = self.checkpoint.as_ref() {
            records.push(historical_summary_record(checkpoint));
            source_refs.push(SourceRef::Summary);
            image_consumption.push(ImageConsumption::Consumed);
            for (index, record) in checkpoint.retained.iter().enumerate() {
                records.push(record.clone());
                source_refs.push(SourceRef::Retained(index));
                image_consumption.push(
                    self.retained_image_consumption
                        .get(index)
                        .copied()
                        .unwrap_or_else(|| retained_image_consumption(record)),
                );
            }
        }
        let raw_start = if self.checkpoint.is_some() {
            self.checkpoint_boundary.max(prompt_len)
        } else {
            prompt_len
        };
        for raw_index in raw_start..self.history.len() {
            records.push(retained_record_from_source(
                &self.history[raw_index],
                &self.provenance[raw_index],
            ));
            source_refs.push(SourceRef::Raw(raw_index));
            image_consumption.push(self.provenance[raw_index].image_consumption);
        }
        (records, source_refs, image_consumption)
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

    /// The active checkpoint, for tests that pin its session envelope.
    #[cfg(test)]
    pub(super) const fn checkpoint(&self) -> Option<&CompactionData> {
        self.checkpoint.as_ref()
    }

    pub(super) fn append_developer_context(
        &mut self,
        contexts: Vec<String>,
        source_task: Option<&str>,
    ) {
        self.align_provenance_for_test_history();
        let timestamp = chrono::Utc::now();
        for content in contexts {
            if content.is_empty() {
                continue;
            }
            self.history.push(ConversationItem::Message {
                role: Role::Developer,
                content,
                id: None,
                status: None,
                timestamp: Some(timestamp),
                images: Vec::new(),
            });
            self.provenance
                .push(RecordProvenance::consumed(source_task.map(str::to_owned)));
        }
        self.assert_provenance_aligned();
    }

    /// Restore a persisted conversation, repairing tool calls the previous
    /// process left without an output.
    ///
    /// A process that ends between persisting a `FunctionCall` and persisting
    /// its `FunctionCallOutput` leaves history that providers reject, so each
    /// unmatched call gains an explicit failed output before the next request.
    /// The repair items are also queued for persistence so later restores see
    /// the same well-formed history.
    pub(super) fn with_restored_history(
        &mut self,
        messages: Vec<ConversationItem>,
    ) -> anyhow::Result<()> {
        self.with_restored_context(ContextSeed {
            messages,
            checkpoint: None,
            boundary: None,
        })
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
            checkpoint,
            boundary,
        } = seed;
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
            .skip(first_non_prompt)
            .map(RecordProvenance::restored)
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
        self.align_provenance_for_test_history();
        let item = ConversationItem::Message {
            role: Role::User,
            content,
            id: None,
            status: None,
            timestamp: Some(chrono::Utc::now()),
            images: Vec::new(),
        };
        self.history.push(item.clone());
        self.provenance
            .push(RecordProvenance::consumed(source_task.map(str::to_owned)).with_origin(origin));
        self.assert_provenance_aligned();
        item
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "checkpoint preparation consumes this evidence in Milestone 2"
        )
    )]
    pub(super) fn extend_turn_items(&mut self, items: Vec<ConversationItem>) {
        let provenance: Vec<RecordProvenance> = items
            .iter()
            .map(|item| RecordProvenance::live(item, None, None))
            .collect();
        self.extend_turn_items_with_provenance(items, provenance);
    }

    /// Append assistant-turn items with the exact provenance captured for each.
    pub(super) fn extend_turn_items_with_provenance(
        &mut self,
        items: Vec<ConversationItem>,
        provenance: Vec<RecordProvenance>,
    ) {
        self.align_provenance_for_test_history();
        assert_eq!(
            items.len(),
            provenance.len(),
            "every appended turn item needs one provenance entry"
        );
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
        self.align_provenance_for_test_history();
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
        call_id: String,
        output: String,
        images: Vec<ImagePart>,
        provenance: RecordProvenance,
    ) -> ConversationItem {
        self.align_provenance_for_test_history();
        let item = ConversationItem::FunctionCallOutput {
            call_id,
            output,
            timestamp: Some(chrono::Utc::now()),
            images,
        };
        self.history.push(item.clone());
        self.provenance.push(provenance);
        self.assert_provenance_aligned();
        item
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
    }

    /// Test helpers may push directly through [`Self::history_mut`]. Backfill
    /// only in test builds so production misalignment still trips the debug
    /// assertion instead of inventing evidence.
    #[cfg(test)]
    fn align_provenance_for_test_history(&mut self) {
        if self.provenance.len() >= self.history.len() {
            return;
        }
        let missing: Vec<RecordProvenance> = self.history[self.provenance.len()..]
            .iter()
            .map(|item| RecordProvenance::live(item, None, None))
            .collect();
        self.provenance.extend(missing);
    }

    #[cfg(not(test))]
    #[expect(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "the signature mirrors the test-only backfill so call sites stay identical"
    )]
    const fn align_provenance_for_test_history(&mut self) {}

    #[cfg(test)]
    pub(super) const fn history_mut(&mut self) -> &mut Vec<ConversationItem> {
        &mut self.history
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
            output: output.clone(),
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
            output: incomplete_tool_call_repair_output(name, call_id),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ReasoningSummary;
    use crate::types::compaction::ContextSeed;
    use crate::types::compaction::test_support;

    #[test]
    fn resolve_assistant_message_with_assistant_message() {
        let items = vec![ConversationItem::Message {
            role: Role::Assistant,
            content: "Hello!".to_string(),
            id: Some("msg-1".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
            images: Vec::new(),
        }];
        let content = resolve_assistant_message(&items);
        assert_eq!(content, Some("Hello!".to_string()));
    }

    #[test]
    fn resolve_assistant_message_truncated_with_reasoning() {
        let items = vec![ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: Some(vec![ReasoningSummary::summary_text("thinking...")]),
            encrypted_content: None,
            content: None,
            timestamp: None,
        }];
        let content = resolve_assistant_message(&items);
        assert!(content.is_none());
    }

    #[test]
    fn resolve_assistant_message_ignores_empty_assistant_message() {
        let items = vec![assistant_message(" \n")];
        let content = resolve_assistant_message(&items);
        assert!(content.is_none());
    }

    fn assistant_message(content: &str) -> ConversationItem {
        ConversationItem::Message {
            role: Role::Assistant,
            content: content.to_string(),
            id: Some("msg-1".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
            images: Vec::new(),
        }
    }

    fn image() -> ImagePart {
        ImagePart {
            media_type: "image/jpeg".to_string(),
            data_base64: "AAAA".to_string(),
            content_id: None,
        }
    }

    #[test]
    fn drop_images_clears_message_and_tool_output_images() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        state.extend_turn_items(vec![
            ConversationItem::Message {
                role: Role::User,
                content: "look".to_string(),
                id: None,
                status: None,
                timestamp: None,
                images: vec![image()],
            },
            ConversationItem::FunctionCallOutput {
                call_id: "call-1".to_string(),
                output: "ReadImage /tmp/a.jpg".to_string(),
                timestamp: None,
                images: vec![image(), image()],
            },
            assistant_message("seen"),
        ]);

        assert_eq!(state.drop_images(), 3);
        assert_eq!(state.drop_images(), 0);

        assert!(
            state
                .provenance()
                .iter()
                .all(|provenance| provenance.image_consumption == ImageConsumption::Consumed),
            "dropping images consumes every cleared record"
        );

        let images: Vec<usize> = state
            .history()
            .iter()
            .filter_map(|item| match item {
                ConversationItem::Message { images, .. }
                | ConversationItem::FunctionCallOutput { images, .. } => Some(images.len()),
                ConversationItem::FunctionCall { .. } | ConversationItem::Reasoning { .. } => None,
            })
            .collect();
        assert_eq!(images, vec![0, 0, 0, 0]);
        assert_eq!(
            state.history().len(),
            4,
            "dropping images must not remove the text they accompanied"
        );
    }

    #[test]
    fn provenance_starts_consumed_for_the_prompt_prefix() {
        let state = ConversationState::new(&[
            (Role::System, "sys".to_string()),
            (Role::Developer, "dev".to_string()),
        ]);

        assert_eq!(state.provenance().len(), 2);
        for provenance in state.provenance() {
            assert_eq!(provenance.origin, None);
            assert_eq!(provenance.source_task, None);
            assert_eq!(provenance.skill_read, None);
            assert_eq!(provenance.replay, None);
            assert_eq!(provenance.image_consumption, ImageConsumption::Consumed);
        }
        assert!(
            state.retirable_raw_source_records().next().is_none(),
            "prompt context is never a retirement candidate"
        );
    }

    #[test]
    fn live_user_inputs_record_origin_and_source_task() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        let user = state.push_user_message("real request".to_string(), Some("task-1"));
        let synthetic = state.push_synthetic_user_message("corrective".to_string(), Some("task-1"));

        assert!(matches!(
            user,
            ConversationItem::Message {
                role: Role::User,
                ..
            }
        ));
        assert!(matches!(
            synthetic,
            ConversationItem::Message {
                role: Role::User,
                ..
            }
        ));
        let provenance = state.provenance();
        assert_eq!(provenance[1].origin, Some(MessageOrigin::User));
        assert_eq!(provenance[1].source_task.as_deref(), Some("task-1"));
        assert_eq!(provenance[2].origin, Some(MessageOrigin::Synthetic));
        assert_eq!(provenance[2].source_task.as_deref(), Some("task-1"));
        assert_eq!(provenance[2].image_consumption, ImageConsumption::Consumed);
    }

    #[test]
    fn developer_context_records_the_task_without_a_user_origin() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        state.append_developer_context(
            vec!["hook context".to_string(), String::new()],
            Some("task-1"),
        );

        assert_eq!(state.provenance().len(), 2, "empty context is not appended");
        let provenance = &state.provenance()[1];
        assert_eq!(provenance.origin, None);
        assert_eq!(provenance.source_task.as_deref(), Some("task-1"));
        assert_eq!(provenance.image_consumption, ImageConsumption::Consumed);
    }

    #[test]
    fn live_tool_output_records_replay_skill_and_image_evidence() {
        let skill_read = SkillRead {
            name: "demo".to_string(),
            path: std::path::PathBuf::from("/skills/demo/SKILL.md"),
        };
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        state.push_tool_output(
            "call-1".to_string(),
            "image output".to_string(),
            vec![image()],
            RecordProvenance::live_tool_output(
                Some("task-1".to_string()),
                Some(ReplaySafety::Safe),
                Some(skill_read.clone()),
                true,
            ),
        );
        state.push_tool_output(
            "call-2".to_string(),
            "text output".to_string(),
            Vec::new(),
            RecordProvenance::live_tool_output(
                Some("task-1".to_string()),
                Some(ReplaySafety::Never),
                None,
                false,
            ),
        );

        let provenance = state.provenance();
        assert_eq!(provenance[1].replay, Some(ReplaySafety::Safe));
        assert_eq!(provenance[1].skill_read, Some(skill_read));
        assert_eq!(
            provenance[1].image_consumption,
            ImageConsumption::Unconsumed
        );
        assert_eq!(provenance[2].replay, Some(ReplaySafety::Never));
        assert_eq!(provenance[2].skill_read, None);
        assert_eq!(provenance[2].image_consumption, ImageConsumption::Consumed);
    }

    #[test]
    fn turn_items_record_the_replay_declaration_and_task() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        let item = function_call("call-1", "Read");
        let provenance =
            RecordProvenance::live(&item, Some("task-1".to_string()), Some(ReplaySafety::Safe));

        state.extend_turn_items_with_provenance(vec![item], vec![provenance]);

        let provenance = &state.provenance()[1];
        assert_eq!(provenance.source_task.as_deref(), Some("task-1"));
        assert_eq!(provenance.replay, Some(ReplaySafety::Safe));
        assert_eq!(provenance.image_consumption, ImageConsumption::Consumed);
    }

    #[test]
    #[should_panic(expected = "every appended turn item needs one provenance entry")]
    fn turn_items_require_matching_provenance() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        state.extend_turn_items_with_provenance(vec![assistant_message("answer")], Vec::new());
    }

    #[test]
    fn retirable_raw_source_records_follow_oldest_first_source_order() {
        let mut state = ConversationState::new(&[
            (Role::System, "sys".to_string()),
            (Role::Developer, "dev".to_string()),
        ]);
        state.push_user_message("request".to_string(), Some("task-1"));
        let answer = assistant_message("answer");
        let answer_provenance = RecordProvenance::live(&answer, Some("task-1".to_string()), None);
        state.extend_turn_items_with_provenance(vec![answer], vec![answer_provenance]);
        state.push_tool_output(
            "call-1".to_string(),
            "tool output".to_string(),
            Vec::new(),
            RecordProvenance::live_tool_output(Some("task-1".to_string()), None, None, false),
        );

        let records: Vec<_> = state.retirable_raw_source_records().collect();
        assert_eq!(
            records
                .iter()
                .map(|record| record.raw_index)
                .collect::<Vec<_>>(),
            vec![2, 3, 4],
            "the prompt prefix is excluded and raw history order is preserved"
        );
        assert_eq!(records[0].provenance.origin, Some(MessageOrigin::User));
        assert_eq!(records[0].provenance.source_task.as_deref(), Some("task-1"));
        assert!(matches!(
            records[0].item,
            ConversationItem::Message { content, .. } if content == "request"
        ));
        assert_eq!(records[2].provenance.source_task.as_deref(), Some("task-1"));
    }

    #[test]
    fn preparation_source_preserves_live_evidence_and_invocation_pins() {
        let (state, invocation_start, current_user_raw, skill_read) = live_preparation_state();
        let source = state
            .preparation_source(current_user_raw, invocation_start, "task-1")
            .unwrap();

        assert_live_source_shape(&source);
        assert_live_source_invocation_pins(&source);
        assert_live_source_records(&source, &skill_read);
    }

    fn live_preparation_state() -> (ConversationState, usize, usize, SkillRead) {
        let mut state = ConversationState::new(&[
            (Role::System, "sys".to_string()),
            (Role::Developer, "dev".to_string()),
        ]);
        state.push_user_message("prior request".to_string(), Some("task-0"));
        state.extend_turn_items(vec![assistant_message("prior answer")]);
        state.append_developer_context(vec!["hook context".to_string()], Some("task-1"));
        let invocation_start = state.history().len();
        let current_user_raw = invocation_start;
        state.push_user_message("request".to_string(), Some("task-1"));
        state.push_synthetic_user_message("correction".to_string(), Some("task-1"));
        let call = function_call("call-1", "Read");
        let call_provenance =
            RecordProvenance::live(&call, Some("task-1".to_string()), Some(ReplaySafety::Safe));
        state.extend_turn_items_with_provenance(vec![call], vec![call_provenance]);
        let skill_read = SkillRead {
            name: "demo".to_string(),
            path: std::path::PathBuf::from("/skills/demo/SKILL.md"),
        };
        state.push_tool_output(
            "call-1".to_string(),
            "skill instructions".to_string(),
            Vec::new(),
            RecordProvenance::live_tool_output(
                Some("task-1".to_string()),
                Some(ReplaySafety::Safe),
                Some(skill_read.clone()),
                false,
            ),
        );
        state.push_tool_output(
            "call-2".to_string(),
            "image output".to_string(),
            vec![image()],
            RecordProvenance::live_tool_output(
                Some("task-1".to_string()),
                Some(ReplaySafety::Never),
                None,
                true,
            ),
        );
        (state, invocation_start, current_user_raw, skill_read)
    }

    fn assert_live_source_shape(source: &PreparationSource) {
        assert_eq!(source.prompt_prefix_len, 2);
        assert_eq!(
            source.source_refs,
            vec![
                SourceRef::Prompt,
                SourceRef::Prompt,
                SourceRef::Raw(2),
                SourceRef::Raw(3),
                SourceRef::Raw(4),
                SourceRef::Raw(5),
                SourceRef::Raw(6),
                SourceRef::Raw(7),
                SourceRef::Raw(8),
                SourceRef::Raw(9),
            ]
        );
        assert_eq!(source.records.len(), source.source_refs.len());
        assert_eq!(source.records.len(), source.image_consumption.len());
    }

    fn assert_live_source_invocation_pins(source: &PreparationSource) {
        let user_source = source_index_of(source, SourceRef::Raw(5));
        assert_eq!(source.current_user_input, Some(user_source));
        let expected = [
            (user_source, InvocationPinKind::UserInput),
            (
                source_index_of(source, SourceRef::Raw(4)),
                InvocationPinKind::DeveloperContext,
            ),
            (
                source_index_of(source, SourceRef::Raw(6)),
                InvocationPinKind::SyntheticInput,
            ),
        ];
        for (source_index, kind) in expected {
            assert!(
                source
                    .invocation_pins
                    .contains(&InvocationPin { source_index, kind }),
                "missing {kind:?} pin at source index {source_index}"
            );
        }
        assert_eq!(source.records[user_source].pin, Some(PinReason::UserInput));
        assert_eq!(
            source.records[user_source].origin,
            Some(MessageOrigin::User)
        );
        let synthetic_source = source_index_of(source, SourceRef::Raw(6));
        assert_eq!(
            source.records[synthetic_source].pin,
            Some(PinReason::SyntheticInput)
        );
        assert_eq!(
            source.records[synthetic_source].origin,
            Some(MessageOrigin::Synthetic)
        );
        assert_eq!(
            source.records[source_index_of(source, SourceRef::Raw(4))].pin,
            Some(PinReason::DeveloperContext)
        );
    }

    fn assert_live_source_records(source: &PreparationSource, skill_read: &SkillRead) {
        let call_record = &source.records[source_index_of(source, SourceRef::Raw(7))];
        assert!(matches!(
            &call_record.record,
            RetainedConversationRecord::FunctionCall(data)
                if data.replay == Some(ReplaySafety::Safe) && data.arguments_parse_error.is_none()
        ));
        let skill_record = &source.records[source_index_of(source, SourceRef::Raw(8))];
        assert_eq!(skill_record.skill_read.as_ref(), Some(skill_read));
        assert!(matches!(
            skill_record.pin,
            Some(PinReason::SkillInstructions { .. })
        ));
        let image_source = source_index_of(source, SourceRef::Raw(9));
        assert_eq!(
            source.records[image_source].pin,
            Some(PinReason::UnconsumedImage)
        );
        assert_eq!(
            source.image_consumption[image_source],
            ImageConsumption::Unconsumed
        );
        assert!(
            source.image_consumption[..image_source]
                .iter()
                .all(|state| *state == ImageConsumption::Consumed)
        );
    }

    #[test]
    fn preparation_source_keeps_the_checkpoint_tail_and_clears_stale_pins() {
        let state = checkpoint_preparation_state();
        let source = state.preparation_source(1, 1, "task-new").unwrap();

        assert_retained_source_shape(&source);
        assert_retained_pin_normalization(&source);
        assert_retained_image_evidence(&source);
    }

    fn checkpoint_preparation_state() -> ConversationState {
        let mut old_developer = test_support::message(Role::Developer, "old developer");
        old_developer.pin = Some(PinReason::DeveloperContext);
        old_developer.source_task = Some("task-old".to_string());
        let mut current_developer = test_support::message(Role::Developer, "current developer");
        current_developer.pin = Some(PinReason::DeveloperContext);
        current_developer.source_task = Some("task-new".to_string());
        let mut unpinned_developer = test_support::message(Role::Developer, "unpinned developer");
        unpinned_developer.source_task = Some("task-new".to_string());
        let mut user = test_support::as_user_input(test_support::message(Role::User, "request"));
        user.source_task = Some("task-new".to_string());
        let mut skill_exchange = test_support::exchange("skill-call", "skill instructions");
        skill_exchange[1] = test_support::as_skill_read(skill_exchange[1].clone(), "demo");
        skill_exchange[1].source_task = Some("task-old".to_string());
        let unconsumed = retained_image_exchange(
            "image-1",
            Some(PinReason::UnconsumedImage),
            Some("task-old"),
        );
        let consumed = retained_image_exchange("image-2", None, Some("task-old"));
        let mut retained = vec![old_developer, current_developer, unpinned_developer, user];
        retained.extend(skill_exchange);
        retained.extend(unconsumed);
        retained.extend(consumed);
        let checkpoint = test_support::checkpoint("checkpoint-1", "old summary", retained);

        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        let current_user_raw = state.history().len();
        state.push_user_message("request".to_string(), Some("task-new"));
        state.activate_checkpoint(checkpoint).unwrap();
        assert_eq!(current_user_raw, 1);
        state
    }

    fn assert_retained_source_shape(source: &PreparationSource) {
        assert_eq!(source.prompt_prefix_len, 1);
        assert_eq!(
            source.source_refs[..2],
            [SourceRef::Prompt, SourceRef::Summary]
        );
        assert!(matches!(
            source.source_refs.last(),
            Some(SourceRef::Retained(9))
        ));
        let summary_count = source
            .records
            .iter()
            .filter(|record| {
                matches!(
                    &record.record,
                    RetainedConversationRecord::Message(data)
                        if data.content.starts_with(crate::types::compaction::HISTORICAL_SUMMARY_MARKER)
                )
            })
            .count();
        assert_eq!(summary_count, 1, "the active summary appears exactly once");
    }

    fn assert_retained_pin_normalization(source: &PreparationSource) {
        let old_developer_source = source_index_of(source, SourceRef::Retained(0));
        let current_developer_source = source_index_of(source, SourceRef::Retained(1));
        let unpinned_developer_source = source_index_of(source, SourceRef::Retained(2));
        let user_source = source_index_of(source, SourceRef::Retained(3));
        let skill_source = source_index_of(source, SourceRef::Retained(5));
        assert_eq!(source.current_user_input, Some(user_source));
        assert_eq!(source.records[old_developer_source].pin, None);
        assert_eq!(
            source.records[current_developer_source].pin,
            Some(PinReason::DeveloperContext)
        );
        assert_eq!(
            source
                .invocation_pins
                .iter()
                .filter(|pin| {
                    pin.source_index == current_developer_source
                        && pin.kind == InvocationPinKind::DeveloperContext
                })
                .count(),
            1
        );
        assert_eq!(
            source.records[unpinned_developer_source].pin,
            Some(PinReason::DeveloperContext)
        );
        assert_eq!(source.records[user_source].pin, Some(PinReason::UserInput));
        assert!(source.invocation_pins.contains(&InvocationPin {
            source_index: user_source,
            kind: InvocationPinKind::UserInput,
        }));
        assert_eq!(source.records[skill_source].pin, None);
        assert!(source.records[skill_source].skill_read.is_some());
    }

    fn assert_retained_image_evidence(source: &PreparationSource) {
        let unconsumed_source = source_index_of(source, SourceRef::Retained(7));
        let consumed_source = source_index_of(source, SourceRef::Retained(9));
        assert_eq!(
            source.records[unconsumed_source].pin,
            Some(PinReason::UnconsumedImage)
        );
        assert_eq!(
            source.image_consumption[unconsumed_source],
            ImageConsumption::Unconsumed
        );
        assert_eq!(source.records[consumed_source].pin, None);
        assert_eq!(
            source.image_consumption[consumed_source],
            ImageConsumption::Consumed
        );
    }

    #[test]
    fn activation_seeds_retained_images_and_drop_images_consumes_them() {
        let checkpoint = test_support::checkpoint(
            "checkpoint-1",
            "summary",
            retained_image_exchange("image-1", Some(PinReason::UnconsumedImage), None)
                .into_iter()
                .chain(retained_image_exchange("image-2", None, None))
                .collect(),
        );
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        state.activate_checkpoint(checkpoint).unwrap();

        assert_eq!(
            state.retained_image_consumption,
            vec![
                ImageConsumption::Consumed,
                ImageConsumption::Unconsumed,
                ImageConsumption::Consumed,
                ImageConsumption::Consumed,
            ]
        );
        assert_eq!(state.drop_images(), 2);
        assert_eq!(
            state.retained_image_consumption,
            vec![ImageConsumption::Consumed; 4]
        );
        let retained = &state.checkpoint().unwrap().retained;
        assert!(matches!(
            &retained[1].record,
            RetainedConversationRecord::FunctionCallOutput(data) if data.images.is_empty()
        ));
        assert_eq!(retained[1].pin, None);
    }

    #[test]
    fn mark_images_consumed_updates_raw_and_retained_evidence() {
        let checkpoint = test_support::checkpoint(
            "checkpoint-1",
            "summary",
            retained_image_exchange("retained-image", Some(PinReason::UnconsumedImage), None),
        );
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        state.activate_checkpoint(checkpoint).unwrap();
        state.push_tool_output(
            "raw-image".to_string(),
            "image".to_string(),
            vec![image()],
            RecordProvenance::live_tool_output(Some("task-1".to_string()), None, None, true),
        );
        let raw_index = state.history().len() - 1;

        state.mark_images_consumed(&[SourceRef::Retained(1), SourceRef::Raw(raw_index)]);

        assert_eq!(
            state.provenance()[raw_index].image_consumption,
            ImageConsumption::Consumed
        );
        assert_eq!(
            state.retained_image_consumption[1],
            ImageConsumption::Consumed
        );
        assert_eq!(state.checkpoint().unwrap().retained[1].pin, None);
    }

    #[test]
    fn restored_checkpoint_seeds_retained_image_state() {
        let checkpoint = test_support::checkpoint(
            "checkpoint-1",
            "summary",
            retained_image_exchange("image-1", Some(PinReason::UnconsumedImage), None),
        );
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        state
            .with_restored_context(restored_seed(
                checkpoint,
                vec![user_message("retired request")],
                1,
            ))
            .unwrap();

        assert_eq!(
            state.retained_image_consumption,
            vec![ImageConsumption::Consumed, ImageConsumption::Unconsumed]
        );
    }

    #[test]
    fn preparation_source_aligns_the_prompt_prefix_with_the_projector() {
        let mut state = ConversationState::new(&[
            (Role::System, "sys".to_string()),
            (Role::Developer, "dev".to_string()),
        ]);
        state.append_developer_context(vec!["leading hook context".to_string()], Some("task-1"));
        let current_user_raw = state.history().len();
        state.push_user_message("request".to_string(), Some("task-1"));

        let source = state
            .preparation_source(current_user_raw, current_user_raw, "task-1")
            .unwrap();

        assert_eq!(source.prompt_prefix_len, 3);
        assert_eq!(
            source.source_refs[..4],
            [
                SourceRef::Prompt,
                SourceRef::Prompt,
                SourceRef::Prompt,
                SourceRef::Raw(3),
            ]
        );
        assert!(matches!(
            &source.records[2].record,
            RetainedConversationRecord::Message(data)
                if data.role == Role::Developer && data.content == "leading hook context"
        ));
        assert_eq!(source.current_user_input, Some(3));
    }

    #[test]
    fn preparation_source_rejects_a_pre_boundary_user_without_a_pin() {
        let checkpoint = test_support::checkpoint(
            "checkpoint-1",
            "summary",
            vec![test_support::message(Role::Assistant, "retired answer")],
        );
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        let current_user_raw = state.history().len();
        state.push_user_message("request".to_string(), Some("task-1"));
        state.activate_checkpoint(checkpoint).unwrap();

        let error = state
            .preparation_source(current_user_raw, current_user_raw, "task-1")
            .expect_err("a current user input before the boundary needs checkpoint evidence");

        assert!(error.to_string().contains("no user_input pin"), "{error}");
    }

    fn source_index_of(source: &PreparationSource, target: SourceRef) -> usize {
        source
            .source_refs
            .iter()
            .position(|source_ref| *source_ref == target)
            .unwrap_or_else(|| panic!("missing source ref {target:?}"))
    }

    fn retained_image_exchange(
        call_id: &str,
        pin: Option<PinReason>,
        source_task: Option<&str>,
    ) -> Vec<RetainedRecord> {
        let mut exchange = test_support::exchange(call_id, "image output");
        if let RetainedConversationRecord::FunctionCallOutput(data) = &mut exchange[1].record {
            data.images.push(image());
        }
        exchange[1].pin = pin;
        exchange[1].source_task = source_task.map(str::to_owned);
        exchange
    }

    #[test]
    fn restored_images_start_unknown_and_follow_the_checkpoint_boundary() {
        let checkpoint = test_support::checkpoint("checkpoint-1", "summary", Vec::new());
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        state
            .with_restored_context(restored_seed(
                checkpoint,
                vec![
                    user_message("retired request"),
                    function_call("call-1", "ReadImage"),
                    ConversationItem::FunctionCallOutput {
                        call_id: "call-1".to_string(),
                        output: "image".to_string(),
                        timestamp: None,
                        images: vec![image()],
                    },
                ],
                1,
            ))
            .unwrap();

        assert_eq!(state.provenance().len(), state.history().len());
        assert_eq!(
            state.provenance()[1].image_consumption,
            ImageConsumption::Consumed
        );
        assert_eq!(state.provenance()[1].origin, None);
        assert_eq!(state.provenance()[2].replay, None);
        assert_eq!(
            state.provenance()[3].image_consumption,
            ImageConsumption::Unknown
        );

        state.push_user_message("new input".to_string(), Some("task-2"));
        assert_eq!(
            state
                .retirable_raw_source_records()
                .map(|record| record.raw_index)
                .collect::<Vec<_>>(),
            vec![2, 3, 4],
            "pre-boundary raw records are replaced by the checkpoint"
        );
    }

    #[test]
    fn resolve_assistant_message_from_ignores_prior_turn_messages() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        state.extend_turn_items(vec![assistant_message("prior answer")]);
        let turn_start = state.history().len();

        assert!(state.resolve_assistant_message_from(turn_start).is_none());
        assert_eq!(
            state.resolve_assistant_message_from(0),
            Some("prior answer".to_string())
        );
    }

    #[test]
    fn resolve_assistant_message_from_finds_current_turn_message() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        state.extend_turn_items(vec![assistant_message("prior answer")]);
        let turn_start = state.history().len();
        state.extend_turn_items(vec![assistant_message("current answer")]);

        assert_eq!(
            state.resolve_assistant_message_from(turn_start),
            Some("current answer".to_string())
        );
    }

    #[test]
    fn resolve_assistant_message_from_past_end_is_none() {
        let state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        assert!(state.resolve_assistant_message_from(10).is_none());
    }

    fn user_message(content: &str) -> ConversationItem {
        ConversationItem::Message {
            role: Role::User,
            content: content.to_string(),
            id: None,
            status: None,
            timestamp: None,
            images: Vec::new(),
        }
    }

    fn function_call(call_id: &str, name: &str) -> ConversationItem {
        ConversationItem::FunctionCall {
            id: format!("fc-{call_id}"),
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: "{}".to_string(),
            timestamp: None,
        }
    }

    fn function_call_output(call_id: &str, output: &str) -> ConversationItem {
        ConversationItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: output.to_string(),
            timestamp: None,
            images: Vec::new(),
        }
    }

    fn repaired_call_ids(items: &[ConversationItem]) -> Vec<&str> {
        items
            .iter()
            .map(|item| match item {
                ConversationItem::FunctionCallOutput { call_id, .. } => call_id.as_str(),
                other => panic!("expected a function call output, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn repair_closes_a_single_unmatched_call() {
        let items = vec![
            user_message("list files"),
            assistant_message("running"),
            function_call("call-1", "Bash"),
        ];

        let repairs = repair_items_for_incomplete_calls(&items).unwrap();

        assert_eq!(repairs.len(), 1);
        let ConversationItem::FunctionCallOutput {
            call_id, output, ..
        } = &repairs[0]
        else {
            panic!("expected a function call output");
        };
        assert_eq!(call_id, "call-1");
        assert_eq!(
            output,
            "not executed: the previous cake process ended before Bash(call-1) \
             recorded a result. Assume the tool did not run, and call it again \
             if its result is still needed."
        );
    }

    #[test]
    fn repair_closes_every_unmatched_call_in_issue_order() {
        let items = vec![
            user_message("do two things"),
            function_call("call-1", "Bash"),
            function_call("call-2", "Read"),
        ];

        let repairs = repair_items_for_incomplete_calls(&items).unwrap();

        assert_eq!(repaired_call_ids(&repairs), vec!["call-1", "call-2"]);
    }

    #[test]
    fn repair_keeps_matched_calls_and_closes_only_the_unmatched_one() {
        let items = vec![
            user_message("do two things"),
            function_call("call-1", "Bash"),
            function_call("call-2", "Read"),
            function_call_output("call-1", "ok"),
        ];

        let repairs = repair_items_for_incomplete_calls(&items).unwrap();

        assert_eq!(repaired_call_ids(&repairs), vec!["call-2"]);
    }

    #[test]
    fn repair_leaves_fully_matched_history_unchanged() {
        let items = vec![
            user_message("list files"),
            function_call("call-1", "Bash"),
            function_call_output("call-1", "ok"),
            assistant_message("done"),
        ];

        assert!(
            repair_items_for_incomplete_calls(&items)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn repair_ignores_interleaved_non_tool_items() {
        let items = vec![
            user_message("investigate"),
            ConversationItem::Reasoning {
                id: "r-1".to_string(),
                summary: Some(vec![ReasoningSummary::summary_text("thinking")]),
                encrypted_content: None,
                content: None,
                timestamp: None,
            },
            assistant_message("looking"),
            function_call("call-1", "Bash"),
            function_call_output("call-1", "ok"),
            assistant_message("still looking"),
            function_call("call-2", "Read"),
        ];

        let repairs = repair_items_for_incomplete_calls(&items).unwrap();

        assert_eq!(repaired_call_ids(&repairs), vec!["call-2"]);
    }

    #[test]
    fn repair_accepts_a_call_id_reused_after_its_output() {
        // Providers that number calls per request can legitimately repeat an
        // id across turns; each pair is closed before the next opens.
        let items = vec![
            function_call("call-1", "Bash"),
            function_call_output("call-1", "ok"),
            function_call("call-1", "Read"),
            function_call_output("call-1", "contents"),
        ];

        assert!(
            repair_items_for_incomplete_calls(&items)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn repair_rejects_two_unfinished_calls_sharing_a_call_id() {
        let items = vec![
            function_call("call-1", "Bash"),
            function_call("call-1", "Read"),
        ];

        let error = repair_items_for_incomplete_calls(&items).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("two unfinished function calls share call_id 'call-1'"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn repair_rejects_an_output_that_precedes_its_call() {
        let items = vec![
            function_call_output("call-1", "ok"),
            function_call("call-1", "Bash"),
        ];

        let error = repair_items_for_incomplete_calls(&items).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("output for call_id 'call-1' has no preceding unmatched function call"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn restored_history_appends_and_queues_repairs() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        state
            .with_restored_history(vec![user_message("go"), function_call("call-1", "Bash")])
            .unwrap();

        assert_eq!(state.history().len(), 4);
        assert!(matches!(
            state.history()[3],
            ConversationItem::FunctionCallOutput { .. }
        ));
        assert_eq!(
            repaired_call_ids(&state.take_pending_repairs()),
            vec!["call-1"]
        );
        assert!(state.take_pending_repairs().is_empty());
    }

    #[test]
    fn restoring_already_repaired_history_is_idempotent() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        state
            .with_restored_history(vec![user_message("go"), function_call("call-1", "Bash")])
            .unwrap();
        let repaired = state.history()[1..].to_vec();

        // A later resume reads back the repaired history from the session file.
        let mut resumed = ConversationState::new(&[(Role::System, "sys".to_string())]);
        resumed.with_restored_history(repaired.clone()).unwrap();

        assert!(resumed.take_pending_repairs().is_empty());
        assert_eq!(resumed.history().len(), 1 + repaired.len());
    }

    #[test]
    fn restored_history_rejects_ambiguous_pairing() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        let error = state
            .with_restored_history(vec![
                function_call("call-1", "Bash"),
                function_call("call-1", "Read"),
            ])
            .unwrap_err();

        assert!(error.to_string().contains("Corrupt session history"));
        // The history is left untouched when restoration fails.
        assert_eq!(state.history().len(), 1);
    }

    fn assistant_message_item(content: &str) -> ConversationItem {
        ConversationItem::Message {
            role: Role::Assistant,
            content: content.to_string(),
            id: Some("msg-1".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
            images: Vec::new(),
        }
    }

    fn restored_seed(
        checkpoint: crate::types::CompactionData,
        messages: Vec<ConversationItem>,
        boundary: usize,
    ) -> ContextSeed {
        ContextSeed {
            messages,
            checkpoint: Some(checkpoint),
            boundary: Some(boundary),
        }
    }

    fn projected_contents(items: &[ConversationItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                ConversationItem::Message { content, .. } => content.clone(),
                ConversationItem::Reasoning { .. } => "<reasoning>".to_string(),
                ConversationItem::FunctionCall { call_id, .. } => format!("call:{call_id}"),
                ConversationItem::FunctionCallOutput { call_id, .. } => {
                    format!("output:{call_id}")
                },
            })
            .collect()
    }

    #[test]
    fn restored_context_projects_the_checkpoint_and_keeps_raw_history() {
        let checkpoint = test_support::checkpoint(
            "checkpoint-1",
            "retired summary",
            test_support::exchange("call-1", "tool output"),
        );
        let mut state = ConversationState::new(&[
            (Role::System, "fresh system".to_string()),
            (Role::Developer, "fresh developer".to_string()),
        ]);
        state
            .with_restored_context(restored_seed(
                checkpoint,
                vec![
                    user_message("retired request"),
                    assistant_message_item("retired answer"),
                    user_message("new request"),
                ],
                2,
            ))
            .unwrap();
        assert!(state.has_checkpoint());
        assert_eq!(
            state.history().len(),
            5,
            "raw history keeps every restored record"
        );

        let projected = state.request_context();

        assert_eq!(
            projected_contents(&projected),
            vec![
                "fresh system".to_string(),
                "fresh developer".to_string(),
                format!(
                    "{}\n\nretired summary",
                    crate::types::compaction::HISTORICAL_SUMMARY_MARKER
                ),
                "call:call-1".to_string(),
                "output:call-1".to_string(),
                "new request".to_string(),
            ],
            "retired raw records are replaced by the checkpoint, once"
        );
        assert!(!projected_contents(&projected).contains(&"retired answer".to_string()));
    }

    #[test]
    fn restored_checkpoint_boundary_follows_the_replaced_prompt_prefix() {
        let checkpoint = test_support::checkpoint("checkpoint-1", "summary", Vec::new());
        let mut state = ConversationState::new(&[(Role::System, "fresh system".to_string())]);
        state
            .with_restored_context(restored_seed(
                checkpoint,
                vec![
                    // The historical developer message is dropped and rebuilt
                    // fresh, so the positional boundary shifts by one.
                    ConversationItem::Message {
                        role: Role::Developer,
                        content: "old hook context".to_string(),
                        id: None,
                        status: None,
                        timestamp: None,
                        images: Vec::new(),
                    },
                    user_message("retired request"),
                    assistant_message_item("retained answer"),
                    user_message("new request"),
                ],
                3,
            ))
            .unwrap();

        let projected = state.request_context();

        assert_eq!(
            projected_contents(&projected),
            vec![
                "fresh system".to_string(),
                format!(
                    "{}\n\nsummary",
                    crate::types::compaction::HISTORICAL_SUMMARY_MARKER
                ),
                "new request".to_string(),
            ],
            "only records after the derived boundary are sent as-is"
        );
    }

    #[test]
    fn new_user_input_after_restore_stays_after_retained_exchanges() {
        let checkpoint = test_support::checkpoint(
            "checkpoint-1",
            "summary",
            test_support::exchange("call-1", "tool output"),
        );
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        state
            .with_restored_context(restored_seed(
                checkpoint,
                vec![user_message("retired request")],
                1,
            ))
            .unwrap();

        state.push_user_message("new user input".to_string(), None);
        let projected = state.request_context();

        assert_eq!(
            projected_contents(&projected).last().map(String::as_str),
            Some("new user input"),
            "a fresh request must follow retained exchanges from earlier tasks"
        );
    }

    #[test]
    fn restored_checkpoint_rejects_an_unfinished_call_before_the_boundary() {
        let checkpoint = test_support::checkpoint("checkpoint-1", "summary", Vec::new());
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        let error = state
            .with_restored_context(restored_seed(
                checkpoint,
                vec![
                    user_message("retired request"),
                    function_call("call-1", "Bash"),
                ],
                2,
            ))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("checkpoint retires the unfinished function call 'call-1'"),
            "unexpected error: {error}"
        );
        assert_eq!(
            state.history().len(),
            1,
            "a rejected restore leaves history untouched"
        );
    }

    #[test]
    fn restored_context_rejects_a_boundary_beyond_the_seed() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        let error = state
            .with_restored_context(restored_seed(
                test_support::checkpoint("checkpoint-1", "summary", Vec::new()),
                vec![user_message("go")],
                2,
            ))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("restored context boundary 2 exceeds its 1 history records"),
            "unexpected error: {error}"
        );
        assert_eq!(
            state.history().len(),
            1,
            "a rejected restore leaves history untouched"
        );
        assert!(!state.has_checkpoint());
    }

    #[test]
    fn restored_context_requires_a_boundary_with_a_checkpoint() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        let error = state
            .with_restored_context(ContextSeed {
                messages: vec![user_message("go")],
                checkpoint: Some(test_support::checkpoint(
                    "checkpoint-1",
                    "summary",
                    Vec::new(),
                )),
                boundary: None,
            })
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("boundary and checkpoint must be provided together"),
            "unexpected error: {error}"
        );
    }
}
