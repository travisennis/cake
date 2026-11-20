//! Automatic pre-request and pressure-recovery compaction orchestration.
//!
//! [`Agent::maybe_compact_before_request`] runs at the top of the send loop,
//! after limits and MCP refresh, so every ordinary provider request sees a
//! projection that fits the configured policy. [`Agent::compact_after_pressure`]
//! runs once after a classified context-pressure failure and commits an
//! overflow-triggered checkpoint before the intended request is retried. Both
//! paths share preparation, summarization, and lifecycle ordering: a prepared
//! candidate emits `compaction_start`, a successful checkpoint is persisted and
//! activated, and only then does `compaction_end` report `committed`.

use chrono::Utc;
use uuid::Uuid;

use crate::clients::agent_state::PreparationSource;
use crate::clients::compaction::{
    self, AgentSummaryCaller, DEFAULT_SUMMARY_TIMEOUT, FixedRequest, Preparation, PreparationInput,
    PreparationMeasure, PrepareOutcome, PreparedCompaction, SUMMARY_INSTRUCTION_VERSION,
};
use crate::types::compaction::{
    CompactionEndData, CompactionStartData, CompactionStatus, CompactionTrigger, ModelIdentity,
    bounded_compaction_reason,
};
use crate::types::{CutOffError, StreamRecord};

use crate::clients::agent::agent_loop::TurnMode;
use crate::clients::agent::{Agent, record_turn_usage};

/// Per-invocation suppression and source anchors for proactive compaction.
///
/// Reset by every [`Agent::send`]. `current_user_raw` keeps the raw index of
/// the real user input, and `invocation_start` is the first raw record of this
/// invocation; the preparation source converts both into explicit source-index
/// pins on every request. `retry_at_tokens` records the projection size that
/// makes another attempt eligible after a failed or skipped one.
#[derive(Debug)]
pub(super) struct InvocationCompactionState {
    pub(super) retry_at_tokens: Option<u64>,
    pub(super) current_user_raw: usize,
    pub(super) invocation_start: usize,
    pub(super) task_id: String,
}

impl InvocationCompactionState {
    pub(super) const fn new(
        current_user_raw: usize,
        invocation_start: usize,
        task_id: String,
    ) -> Self {
        Self {
            retry_at_tokens: None,
            current_user_raw,
            invocation_start,
            task_id,
        }
    }

    /// Whether an earlier attempt failed or was skipped and no retry is due yet.
    #[cfg(test)]
    pub(super) const fn failed(&self) -> bool {
        self.retry_at_tokens.is_some()
    }
}

/// Fraction of the trigger the projection must grow by after a failure before
/// another automatic attempt may run.
const RETRY_GROWTH_DIVISOR: u64 = 10;

impl Agent {
    /// Check and, when needed, compact before the next provider request.
    ///
    /// A configured but disabled policy still checks writer health, then
    /// returns without records. No configured policy returns before any writer
    /// query. After a failed or skipped attempt, another automatic attempt runs
    /// only once the projection has grown by at least a tenth of the trigger
    /// and still reaches it; every request in between is measured and fails
    /// before sending when the projection is over the window.
    ///
    /// # Errors
    ///
    /// Returns a terminal writer cause, a preparation error, a checkpoint
    /// persistence error, or an actionable over-window error.
    pub(super) async fn maybe_compact_before_request(
        &mut self,
        turn_mode: TurnMode,
        invocation: &mut InvocationCompactionState,
    ) -> anyhow::Result<()> {
        if !self.automatic_compaction_ready()? {
            return Ok(());
        }
        if !self.maybe_retry_failed_compaction(turn_mode, invocation)? {
            return Ok(());
        }

        let checkpoint_id = Uuid::new_v4().to_string();
        let preparation = self.prepare_for_request(
            turn_mode,
            invocation,
            checkpoint_id.clone(),
            CompactionTrigger::Threshold,
        )?;
        let _ = self
            .apply_preparation(
                preparation,
                checkpoint_id,
                invocation,
                CompactionTrigger::Threshold,
            )
            .await?;
        Ok(())
    }

    /// Whether automatic compaction may run now.
    ///
    /// A configured but disabled policy still checks writer health, matching
    /// the request path's terminal-writer contract.
    fn automatic_compaction_ready(&self) -> anyhow::Result<bool> {
        if self.compaction_policy.is_none() {
            return Ok(false);
        }
        self.ensure_healthy()?;
        Ok(self
            .resolved_compaction_policy
            .is_some_and(|resolved| resolved.enabled))
    }

    /// Re-check a failed or skipped attempt and clear suppression when due.
    ///
    /// Returns `true` when the projection is due for another attempt or none
    /// was suppressed. A not-yet-grown projection returns `false` after the
    /// over-window check, so the caller sends without preparing.
    fn maybe_retry_failed_compaction(
        &self,
        turn_mode: TurnMode,
        invocation: &mut InvocationCompactionState,
    ) -> anyhow::Result<bool> {
        let Some(retry_at) = invocation.retry_at_tokens else {
            return Ok(true);
        };
        let measure = self.measure_for_request(turn_mode, invocation)?;
        if !measure.trigger_reached || measure.before_tokens < retry_at {
            self.continue_after_failed_attempt(measure, invocation)?;
            return Ok(false);
        }
        invocation.retry_at_tokens = None;
        tracing::info!(
            target: "cake",
            before_tokens = measure.before_tokens,
            retry_at,
            "Context compaction retrying after an earlier failed or skipped attempt"
        );
        Ok(true)
    }

    /// Prepare, summarize, and commit one overflow-triggered checkpoint.
    ///
    /// Returns whether a checkpoint was committed. A missing or disabled
    /// policy, an unknown context window, a no-fit selection, or a failed
    /// summary returns `false`; the caller then ends the invocation with the
    /// neutral context-pressure error instead of retrying the same request.
    pub(super) async fn compact_after_pressure(
        &mut self,
        turn_mode: TurnMode,
        invocation: &mut InvocationCompactionState,
    ) -> anyhow::Result<bool> {
        if !self.automatic_compaction_ready()? || self.context_window().is_none() {
            return Ok(false);
        }

        let checkpoint_id = Uuid::new_v4().to_string();
        let preparation = self.prepare_for_request(
            turn_mode,
            invocation,
            checkpoint_id.clone(),
            CompactionTrigger::Overflow,
        )?;
        self.apply_preparation(
            preparation,
            checkpoint_id,
            invocation,
            CompactionTrigger::Overflow,
        )
        .await
    }

    async fn apply_preparation(
        &mut self,
        preparation: Preparation,
        checkpoint_id: String,
        invocation: &mut InvocationCompactionState,
        trigger: CompactionTrigger,
    ) -> anyhow::Result<bool> {
        let measure = preparation.measure;
        match preparation.outcome {
            PrepareOutcome::NoOp { reason } => {
                if !measure.trigger_reached {
                    return Ok(false);
                }
                self.record_compaction_skip(
                    &checkpoint_id,
                    invocation,
                    trigger,
                    CompactionStatus::NoOp,
                    measure,
                    &reason,
                )
            },
            PrepareOutcome::NoFit { reason } => self.record_compaction_skip(
                &checkpoint_id,
                invocation,
                trigger,
                CompactionStatus::NoFit,
                measure,
                &reason,
            ),
            PrepareOutcome::Prepared(prepared) => {
                self.commit_prepared(*prepared, checkpoint_id, invocation, trigger)
                    .await
            },
        }
    }

    /// Build the exact preparation source for the coming request.
    fn preparation_source(
        &self,
        invocation: &InvocationCompactionState,
    ) -> anyhow::Result<PreparationSource> {
        self.conversation.preparation_source(
            invocation.current_user_raw,
            invocation.invocation_start,
            &invocation.task_id,
        )
    }

    fn prepare_for_request(
        &self,
        turn_mode: TurnMode,
        invocation: &InvocationCompactionState,
        checkpoint_id: String,
        trigger: CompactionTrigger,
    ) -> anyhow::Result<Preparation> {
        let source = self.preparation_source(invocation)?;
        let input =
            self.preparation_input(&source, turn_mode, invocation, checkpoint_id, trigger)?;
        compaction::prepare(input)
    }

    fn measure_for_request(
        &self,
        turn_mode: TurnMode,
        invocation: &InvocationCompactionState,
    ) -> anyhow::Result<PreparationMeasure> {
        let source = self.preparation_source(invocation)?;
        let input = self.preparation_input(
            &source,
            turn_mode,
            invocation,
            String::new(),
            CompactionTrigger::Threshold,
        )?;
        compaction::measure(&input)
    }

    fn preparation_input<'a>(
        &'a self,
        source: &'a PreparationSource,
        turn_mode: TurnMode,
        invocation: &InvocationCompactionState,
        checkpoint_id: String,
        trigger: CompactionTrigger,
    ) -> anyhow::Result<PreparationInput<'a>> {
        let Some(policy) = self.compaction_policy else {
            anyhow::bail!("automatic compaction requires a configured policy");
        };
        Ok(PreparationInput {
            source: &source.records,
            prompt_prefix_len: source.prompt_prefix_len,
            current_user_input: source.current_user_input,
            invocation_pins: &source.invocation_pins,
            image_consumption: &source.image_consumption,
            current_task_id: invocation.task_id.clone(),
            fixed: self.fixed_request_surface(turn_mode),
            context_window: self.context_window(),
            measured: self.last_measurement().cloned(),
            supports_images: self.config.model_config.supports_images,
            image_policy: &self.image_policy,
            checkpoint_id,
            session_id: self.session_id,
            task_id: self.task_id,
            model: self.model_identity(),
            trigger,
            policy,
        })
    }

    /// The fixed token surface the coming request will carry.
    fn fixed_request_surface(&self, turn_mode: TurnMode) -> FixedRequest<'_> {
        let tool_schemas = if turn_mode.tools_disabled() {
            &[]
        } else {
            self.tools.definitions()
        };
        let native_constraint_attached = turn_mode.tools_disabled()
            && self.native_constraint_enabled
            && self.output_schema.is_some();
        let output_schema = if native_constraint_attached {
            self.output_schema.as_deref().map(|schema| &schema.raw)
        } else {
            None
        };
        FixedRequest {
            tool_schemas,
            output_schema,
        }
    }

    fn model_identity(&self) -> ModelIdentity {
        ModelIdentity {
            model: self.config.model_config.model.clone(),
            model_config: self.model_config_name.clone(),
            reasoning_effort: self
                .config
                .model_config
                .reasoning_effort
                .map(|effort| effort.as_str().to_string()),
        }
    }

    /// Continue after a skipped or failed attempt, failing before an
    /// over-window request.
    fn continue_after_failed_attempt(
        &self,
        measure: PreparationMeasure,
        invocation: &InvocationCompactionState,
    ) -> anyhow::Result<()> {
        if self.over_window(measure.before_tokens) {
            return Err(over_window_error(
                self.session_id,
                measure.before_tokens,
                self.context_window().unwrap_or_default(),
                invocation,
            ));
        }
        Ok(())
    }

    /// The projection size that makes another attempt eligible after a failed
    /// or skipped attempt at `before_tokens`.
    ///
    /// A retry waits until the projection grows by at least a tenth of the
    /// trigger, so one transient failure does not repeat on every later turn.
    /// Without a trigger the projection never reaches the trigger again, so no
    /// retry is eligible and the fallback margin is unreachable in practice.
    fn compaction_retry_at(&self, before_tokens: u64) -> u64 {
        let trigger = self
            .resolved_compaction_policy
            .and_then(|policy| policy.trigger_tokens)
            .unwrap_or(before_tokens);
        before_tokens.saturating_add((trigger / RETRY_GROWTH_DIVISOR).max(1))
    }

    fn over_window(&self, estimated_before_tokens: u64) -> bool {
        self.context_window()
            .is_some_and(|window| estimated_before_tokens >= u64::from(window))
    }

    /// Record a no-op or no-fit lifecycle end and require growth before another
    /// attempt.
    fn record_compaction_skip(
        &mut self,
        checkpoint_id: &str,
        invocation: &mut InvocationCompactionState,
        trigger: CompactionTrigger,
        status: CompactionStatus,
        measure: PreparationMeasure,
        reason: &str,
    ) -> anyhow::Result<bool> {
        invocation.retry_at_tokens = Some(self.compaction_retry_at(measure.before_tokens));
        let reason = bounded_compaction_reason(reason);
        self.stream_record(StreamRecord::CompactionEnd(CompactionEndData {
            session_id: self.session_id.to_string(),
            task_id: self.task_id.to_string(),
            checkpoint_id: checkpoint_id.to_string(),
            timestamp: Utc::now(),
            trigger,
            status,
            estimated_before_tokens: Some(measure.before_tokens),
            estimated_after_tokens: None,
            basis: Some(measure.basis),
            retired_count: None,
            retained_count: None,
            reason: Some(reason.clone()),
        }))?;
        self.report_progress(&format!("Context compaction skipped: {reason}"));
        if trigger == CompactionTrigger::Threshold && self.over_window(measure.before_tokens) {
            return Err(over_window_error(
                self.session_id,
                measure.before_tokens,
                self.context_window().unwrap_or_default(),
                invocation,
            ));
        }
        Ok(false)
    }

    /// Summarize a prepared candidate, persist it, and activate it in memory.
    async fn commit_prepared(
        &mut self,
        prepared: PreparedCompaction,
        checkpoint_id: String,
        invocation: &mut InvocationCompactionState,
        trigger: CompactionTrigger,
    ) -> anyhow::Result<bool> {
        let before_tokens = prepared.estimated_input.before_tokens;
        let after_tokens = prepared.estimated_input.after_tokens;
        let basis = prepared.estimated_input.basis;
        let retired_count = prepared
            .retired_exchanges
            .iter()
            .map(Vec::len)
            .fold(0_usize, usize::saturating_add);
        let retained_count = prepared.retained.len();

        self.stream_record(StreamRecord::CompactionStart(CompactionStartData {
            session_id: self.session_id.to_string(),
            task_id: self.task_id.to_string(),
            checkpoint_id: checkpoint_id.clone(),
            timestamp: Utc::now(),
            trigger,
            estimated_before_tokens: before_tokens,
            context_window: self.context_window(),
            summary_instruction_version: SUMMARY_INSTRUCTION_VERSION,
        }))?;
        self.report_progress(&format!(
            "Compacting context: approximately {before_tokens} input tokens"
        ));

        let checkpoint = match self.summarize_prepared(prepared).await {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                tracing::warn!(target: "cake", %error, "Context compaction summary failed");
                return self.record_prepared_failure(
                    &checkpoint_id,
                    invocation,
                    trigger,
                    "summary generation failed",
                    before_tokens,
                    after_tokens,
                    basis,
                    retired_count,
                    retained_count,
                );
            },
        };
        if let Err(error) = checkpoint.validate_structure() {
            tracing::warn!(
                target: "cake",
                %error,
                "Context compaction candidate failed structural validation"
            );
            return self.record_prepared_failure(
                &checkpoint_id,
                invocation,
                trigger,
                "candidate validation failed",
                before_tokens,
                after_tokens,
                basis,
                retired_count,
                retained_count,
            );
        }

        self.stream_record(StreamRecord::Compaction(checkpoint.clone()))?;
        self.activate_checkpoint(checkpoint)?;
        self.last_usage = None;
        self.stream_record(StreamRecord::CompactionEnd(CompactionEndData {
            session_id: self.session_id.to_string(),
            task_id: self.task_id.to_string(),
            checkpoint_id,
            timestamp: Utc::now(),
            trigger,
            status: CompactionStatus::Committed,
            estimated_before_tokens: Some(before_tokens),
            estimated_after_tokens: Some(after_tokens),
            basis: Some(basis),
            retired_count: Some(retired_count),
            retained_count: Some(retained_count),
            reason: None,
        }))?;
        self.report_progress(&format!(
            "Context compacted: approximately {before_tokens} -> {after_tokens} tokens"
        ));
        Ok(true)
    }

    async fn summarize_prepared(
        &mut self,
        prepared: PreparedCompaction,
    ) -> anyhow::Result<crate::types::CompactionData> {
        let health = self.health_callback();
        let telemetry_sink = self.runner_telemetry_sink();
        let mut telemetry = move |event| {
            if let Some(sink) = &telemetry_sink {
                sink.record(event);
            }
        };
        let session_id = self.session_id;
        let task_id = self.task_id;
        let context_window = self.context_window();
        let config = &self.config;
        let summarizer = self.compaction_summarizer_config.as_deref();
        let mut settle = |settlement| {
            record_turn_usage(
                &mut self.observer,
                &mut self.total_usage,
                &mut self.last_usage,
                session_id,
                task_id,
                context_window,
                settlement,
            );
        };
        let mut caller = AgentSummaryCaller::new(
            config,
            summarizer,
            session_id,
            task_id,
            health,
            &mut telemetry,
            &mut settle,
        )?;
        let timeout = self
            .resolved_compaction_policy
            .map_or(DEFAULT_SUMMARY_TIMEOUT, |policy| policy.summary_timeout);
        compaction::summarize(prepared, &mut caller, timeout).await
    }

    /// Emit a failed end record, which is best-effort only when the writer is
    /// still healthy; a terminal write returns the writer's preserved cause.
    #[expect(
        clippy::too_many_arguments,
        reason = "failure metadata mirrors the lifecycle record without a builder"
    )]
    fn record_prepared_failure(
        &mut self,
        checkpoint_id: &str,
        invocation: &mut InvocationCompactionState,
        trigger: CompactionTrigger,
        reason: &str,
        before_tokens: u64,
        after_tokens: u64,
        basis: crate::types::compaction::EstimateBasis,
        retired_count: usize,
        retained_count: usize,
    ) -> anyhow::Result<bool> {
        invocation.retry_at_tokens = Some(self.compaction_retry_at(before_tokens));
        let end = StreamRecord::CompactionEnd(CompactionEndData {
            session_id: self.session_id.to_string(),
            task_id: self.task_id.to_string(),
            checkpoint_id: checkpoint_id.to_string(),
            timestamp: Utc::now(),
            trigger,
            status: CompactionStatus::Failed,
            estimated_before_tokens: Some(before_tokens),
            estimated_after_tokens: Some(after_tokens),
            basis: Some(basis),
            retired_count: Some(retired_count),
            retained_count: Some(retained_count),
            reason: Some(reason.to_string()),
        });
        if let Err(error) = self.stream_record(end) {
            self.ensure_healthy()?;
            return Err(error);
        }
        self.report_progress(&format!("Context compaction failed: {reason}"));
        if trigger == CompactionTrigger::Threshold && self.over_window(before_tokens) {
            return Err(over_window_error(
                self.session_id,
                before_tokens,
                self.context_window().unwrap_or_default(),
                invocation,
            ));
        }
        Ok(false)
    }
}

fn over_window_error(
    session_id: Uuid,
    estimated_before_tokens: u64,
    context_window: u32,
    invocation: &InvocationCompactionState,
) -> anyhow::Error {
    let detail = format!(
        "Context compaction could not reduce the projected request below the configured context \
         window ({context_window} tokens; estimated input {estimated_before_tokens} tokens). \
         Raise or remove the model's `context_window`, or start a new session; resuming this \
         session re-sends the same input. Task: {}. Session: {session_id}.",
        invocation.task_id
    );
    CutOffError::new(detail).into()
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
