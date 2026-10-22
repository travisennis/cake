//! Context-compaction checkpoints and the provider-request projector.
//!
//! A checkpoint is a self-contained, versioned summary plus a retained tail of
//! complete provider exchanges. It is persisted as one `compaction` record in
//! the format-4 append-only session stream, so the session header and its
//! format version do not change; the record carries its own version instead.
//!
//! The retained tail is a restricted typed union of the existing conversation
//! payloads. It cannot hold another checkpoint, prompt context, task boundary,
//! hook audit, usage aggregate, or executable command, so a reader never has to
//! interpret a second conversation schema.
//!
//! [`project_context`] is the one pure projector that turns raw history and the
//! latest checkpoint into the provider request. Live runs and session restore
//! both call it, so resume and fork cannot drift from a live request.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::conversation::{ConversationItem, Role};
use crate::types::session::{FunctionCallData, FunctionCallOutputData, MessageData, ReasoningData};

/// The only checkpoint record version this build writes and reads.
pub const CHECKPOINT_VERSION: u32 = 1;

/// Maximum serialized size of one `compaction` record, including its newline.
pub const MAX_CHECKPOINT_RECORD_BYTES: usize = 8 * 1024 * 1024;

/// Conservative bytes reserved for the `{"type":"compaction",...}` record
/// envelope and its trailing newline when checking the record ceiling.
const RECORD_ENVELOPE_BYTES: usize = 64;

/// Opens the summary when the projector substitutes a checkpoint for retired
/// history. The summary is reference material, not a new user request.
pub const HISTORICAL_SUMMARY_MARKER: &str = "[Historical context summary of earlier work in this session. It is reference material, \
     not a new user request.]";

/// What caused a checkpoint to be written.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTrigger {
    /// An explicit `cake sessions compact` operation.
    Manual,
    /// Estimated input crossed the configured threshold before a request.
    Threshold,
    /// A provider rejected or terminated a request under context pressure.
    Overflow,
}

/// How the recorded input sizes were obtained.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EstimateBasis {
    /// Anchored on the provider's reported input tokens for the request sent.
    MeasuredUsage,
    /// Estimated from content classes, because no matched measurement exists.
    ContentClass,
}

/// Estimated input tokens before and after the checkpoint, with their basis.
///
/// These are estimates, not provider measurements of the projected request.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct EstimatedInput {
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub basis: EstimateBasis,
}

/// The configured model identity a checkpoint was written under.
///
/// Deliberately excludes credentials and fully resolved settings: a checkpoint
/// is session data, not a configuration snapshot.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelIdentity {
    /// Provider model ID, for example `gpt-5.6-luna`.
    pub model: String,
    /// The `[[models]]` entry name, when one was resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_config: Option<String>,
    /// Resolved reasoning effort, when the run configured one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// Who authored a retained user-role message.
///
/// An absent origin on a historical record means unknown: older sessions did
/// not record whether a user-role message came from the user or from Cake.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageOrigin {
    /// Text the user actually submitted.
    User,
    /// Cake-authored synthetic input, such as a schema-correction prompt.
    Synthetic,
}

/// Why a checkpoint retained an exact record or complete exchange.
///
/// This is local checkpoint metadata: it is not a pointer into raw history and
/// never becomes a provider-visible item.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PinReason {
    /// A user input retained exactly as sent.
    UserInput,
    /// Observed skill instructions the exchange must keep verbatim.
    SkillInstructions { name: String, path: PathBuf },
    /// An image result not yet included in an accepted main request.
    UnconsumedImage,
}

/// Provenance for a successful `Read` of a known skill's instructions.
///
/// Recorded on the function-call output independently of the once-per-session
/// `SkillActivated` telemetry, so repeated reads stay identifiable.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SkillRead {
    pub name: String,
    pub path: PathBuf,
}

/// The restricted union of payloads a checkpoint may retain.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RetainedConversationRecord {
    Message(MessageData),
    Reasoning(ReasoningData),
    FunctionCall(FunctionCallData),
    FunctionCallOutput(FunctionCallOutputData),
}

/// One retained payload plus checkpoint-local provenance.
///
/// The provenance fields are additive record metadata. They survive ordinary
/// and recovery forks and never change provider serialization.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RetainedRecord {
    #[serde(flatten)]
    pub record: RetainedConversationRecord,
    /// Why selection may not retire this record or its exchange.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<PinReason>,
    /// The task that owned the retained content, when it is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_task: Option<String>,
    /// Who authored a retained message; absent means unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<MessageOrigin>,
    /// Observed skill-instruction provenance for a successful tool output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_read: Option<SkillRead>,
}

impl RetainedRecord {
    /// Convert the retained payload back into its provider-neutral item.
    pub fn to_conversation_item(&self) -> ConversationItem {
        match &self.record {
            RetainedConversationRecord::Message(data) => ConversationItem::Message {
                role: data.role,
                content: data.content.clone(),
                id: data.id.clone(),
                status: data.status.clone(),
                timestamp: data.timestamp,
                images: data.images.clone(),
            },
            RetainedConversationRecord::Reasoning(data) => ConversationItem::Reasoning {
                id: data.id.clone(),
                summary: data.summary.clone(),
                encrypted_content: data.encrypted_content.clone(),
                content: data.content.clone(),
                timestamp: data.timestamp,
            },
            RetainedConversationRecord::FunctionCall(data) => ConversationItem::FunctionCall {
                id: data.id.clone(),
                call_id: data.call_id.clone(),
                name: data.name.clone(),
                arguments: data.arguments.clone(),
                timestamp: data.timestamp,
            },
            RetainedConversationRecord::FunctionCallOutput(data) => {
                ConversationItem::FunctionCallOutput {
                    call_id: data.call_id.clone(),
                    output: data.output.clone(),
                    timestamp: data.timestamp,
                    images: data.images.clone(),
                }
            },
        }
    }

    /// Local provenance rules: each metadata field belongs to one payload kind.
    fn validate_provenance(&self) -> anyhow::Result<()> {
        let is_message = matches!(&self.record, RetainedConversationRecord::Message(_));
        let is_output = matches!(
            &self.record,
            RetainedConversationRecord::FunctionCallOutput(_)
        );
        anyhow::ensure!(
            is_message || self.origin.is_none(),
            "Compaction checkpoint origin is only valid on a retained message"
        );
        anyhow::ensure!(
            is_output || self.skill_read.is_none(),
            "Compaction checkpoint skill_read is only valid on a retained function call output"
        );
        Ok(())
    }
}

/// One saved summary plus the retained tail that replaces retired history.
#[derive(Debug, Clone, Serialize)]
pub struct CompactionData {
    /// This record's own version; independent of the session `format_version`.
    pub checkpoint_version: u32,
    /// Identity of this checkpoint content.
    pub checkpoint_id: String,
    /// The session that owns this envelope.
    pub session_id: String,
    /// The task that wrote this checkpoint.
    pub task_id: String,
    pub timestamp: DateTime<Utc>,
    pub trigger: CompactionTrigger,
    /// Summary of everything before the retained tail. Never empty.
    pub summary: String,
    /// Complete exchanges plus pinned records, in original source order.
    pub retained: Vec<RetainedRecord>,
    pub model: ModelIdentity,
    /// Version of the summary instruction that produced `summary`.
    pub summary_instruction_version: u32,
    pub estimated_input: EstimatedInput,
}

/// Deserialization twin for [`CompactionData`].
///
/// Readers validate every checkpoint as it is parsed, so an unknown version or
/// structurally unsafe tail fails loudly in every consumer rather than only in
/// the paths that remember to call [`CompactionData::validate`].
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactionDataWire {
    checkpoint_version: u32,
    checkpoint_id: String,
    session_id: String,
    task_id: String,
    timestamp: DateTime<Utc>,
    trigger: CompactionTrigger,
    summary: String,
    retained: Vec<RetainedRecord>,
    model: ModelIdentity,
    summary_instruction_version: u32,
    estimated_input: EstimatedInput,
}

impl From<CompactionDataWire> for CompactionData {
    fn from(wire: CompactionDataWire) -> Self {
        Self {
            checkpoint_version: wire.checkpoint_version,
            checkpoint_id: wire.checkpoint_id,
            session_id: wire.session_id,
            task_id: wire.task_id,
            timestamp: wire.timestamp,
            trigger: wire.trigger,
            summary: wire.summary,
            retained: wire.retained,
            model: wire.model,
            summary_instruction_version: wire.summary_instruction_version,
            estimated_input: wire.estimated_input,
        }
    }
}

impl<'de> Deserialize<'de> for CompactionData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let data = Self::from(CompactionDataWire::deserialize(deserializer)?);
        data.validate_structure()
            .map_err(serde::de::Error::custom)?;
        Ok(data)
    }
}

impl CompactionData {
    /// Validate the checkpoint before it is committed.
    ///
    /// # Errors
    ///
    /// Rejects everything [`Self::validate_structure`] rejects, plus a record
    /// above the serialized size ceiling.
    pub fn validate(&self) -> anyhow::Result<()> {
        self.validate_structure()?;
        let record_bytes = self.serialized_record_len()?;
        anyhow::ensure!(
            record_bytes <= MAX_CHECKPOINT_RECORD_BYTES,
            "Compaction checkpoint record is {record_bytes} bytes; the limit is \
             {MAX_CHECKPOINT_RECORD_BYTES} bytes"
        );
        Ok(())
    }

    /// Validate the checkpoint's version, summary, provenance, and pairing.
    ///
    /// Every read path runs this as the record is parsed, so an unknown
    /// version or an unsafe retained tail fails loudly wherever a checkpoint
    /// enters the process. The size ceiling is a write-side invariant and is
    /// checked by [`Self::validate`] before a commit.
    fn validate_structure(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.checkpoint_version == CHECKPOINT_VERSION,
            "Unsupported compaction checkpoint_version: {} (expected {CHECKPOINT_VERSION})",
            self.checkpoint_version
        );
        anyhow::ensure!(
            !self.summary.trim().is_empty(),
            "Compaction checkpoint summary is empty"
        );
        for retained in &self.retained {
            validate_retained_message(retained)?;
            retained.validate_provenance()?;
        }
        validate_complete_pairing(&self.retained)
    }

    /// The checkpoint's bytes when framed as one JSONL session record.
    fn serialized_record_len(&self) -> anyhow::Result<usize> {
        let payload = serde_json::to_vec(self).map_err(|error| {
            anyhow::anyhow!("Failed to serialize compaction checkpoint: {error}")
        })?;
        Ok(payload.len() + RECORD_ENVELOPE_BYTES)
    }

    /// Copy this checkpoint into another session's envelope.
    ///
    /// Fork seeding keeps the checkpoint's content and identity, and only
    /// rebinds the envelope to the new session so no checkpoint points into
    /// the source file.
    pub fn for_session(&self, session_id: String) -> Self {
        Self {
            session_id,
            ..self.clone()
        }
    }
}

/// Retained messages must keep a role the projector can safely resend.
///
/// Prompt context is rebuilt fresh for every invocation, and tool results only
/// belong in function-call outputs, so neither may appear as a retained message.
fn validate_retained_message(retained: &RetainedRecord) -> anyhow::Result<()> {
    let RetainedConversationRecord::Message(message) = &retained.record else {
        return Ok(());
    };
    anyhow::ensure!(
        !matches!(message.role, Role::System | Role::Tool),
        "Compaction checkpoint retained a {} message; retained messages must be user, \
         developer, or assistant",
        message.role.as_str()
    );
    Ok(())
}

/// Every retained call needs its output, and every output needs its call.
///
/// Provider call IDs repeat after a completed exchange, so pairing walks the
/// retained records in order: each call opens and the next matching output
/// closes it. Two calls open at once, or an output with no open call, makes the
/// tail ambiguous and is rejected rather than guessed.
fn validate_complete_pairing(retained: &[RetainedRecord]) -> anyhow::Result<()> {
    let mut open: Vec<&str> = Vec::new();
    for retained in retained {
        match &retained.record {
            RetainedConversationRecord::FunctionCall(data) => {
                anyhow::ensure!(
                    !open.contains(&data.call_id.as_str()),
                    "Compaction checkpoint retained two unfinished function calls sharing \
                     call_id '{}'",
                    data.call_id
                );
                open.push(&data.call_id);
            },
            RetainedConversationRecord::FunctionCallOutput(data) => {
                let Some(position) = open.iter().position(|call_id| *call_id == data.call_id)
                else {
                    anyhow::bail!(
                        "Compaction checkpoint retained a function call output for call_id \
                         '{}' with no preceding unmatched function call",
                        data.call_id
                    );
                };
                open.remove(position);
            },
            RetainedConversationRecord::Message(_) | RetainedConversationRecord::Reasoning(_) => {},
        }
    }
    if let Some(call_id) = open.first() {
        anyhow::bail!(
            "Compaction checkpoint retained a function call '{call_id}' without its output"
        );
    }
    Ok(())
}

/// Raw history and the latest checkpoint, as restore reads them from a session.
///
/// `messages` is the full raw conversation. `boundary` is the conversation-item
/// count at the latest `compaction` record, so `messages[..boundary]` is
/// represented by `checkpoint` and `messages[boundary..]` is sent as-is.
#[derive(Debug, Clone, Default)]
pub struct ContextSeed {
    pub messages: Vec<ConversationItem>,
    pub checkpoint: Option<CompactionData>,
    pub boundary: Option<usize>,
}

/// Build the provider request from raw history and the latest checkpoint.
///
/// Without a checkpoint this is the raw history, unchanged. With one, the fresh
/// prompt context stays, retired history is replaced by the cycle's summary and
/// retained tail, and everything appended after the checkpoint follows in
/// source order. Records before the positional boundary are never also emitted
/// from raw history, so retained duplicates appear once.
pub fn project_context(
    history: &[ConversationItem],
    checkpoint: Option<&CompactionData>,
    boundary: usize,
) -> Vec<ConversationItem> {
    let Some(checkpoint) = checkpoint else {
        return history.to_vec();
    };
    let boundary = boundary.min(history.len());
    let mut projected = Vec::with_capacity(checkpoint.retained.len() + history.len() + 1);
    projected.extend(prompt_context_prefix(&history[..boundary]));
    projected.push(historical_summary_item(checkpoint));
    projected.extend(
        checkpoint
            .retained
            .iter()
            .map(RetainedRecord::to_conversation_item),
    );
    projected.extend_from_slice(&history[boundary..]);
    projected
}

/// The fresh invocation prompts at the head of raw history survive retirement.
fn prompt_context_prefix(
    items: &[ConversationItem],
) -> impl Iterator<Item = ConversationItem> + '_ {
    items
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
        .cloned()
}

/// The summary as a clearly marked user-role historical message.
fn historical_summary_item(checkpoint: &CompactionData) -> ConversationItem {
    ConversationItem::Message {
        role: Role::User,
        content: format!("{HISTORICAL_SUMMARY_MARKER}\n\n{}", checkpoint.summary),
        id: None,
        status: None,
        timestamp: Some(checkpoint.timestamp),
        images: Vec::new(),
    }
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;

#[cfg(test)]
pub mod test_support {
    use super::*;
    use chrono::Utc;
    use std::path::PathBuf;

    /// A valid checkpoint with the given identity, summary, and retained tail.
    pub fn checkpoint(
        checkpoint_id: &str,
        summary: &str,
        retained: Vec<RetainedRecord>,
    ) -> CompactionData {
        CompactionData {
            checkpoint_version: CHECKPOINT_VERSION,
            checkpoint_id: checkpoint_id.to_string(),
            session_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            task_id: "550e8400-e29b-41d4-a716-446655440001".to_string(),
            timestamp: Utc::now(),
            trigger: CompactionTrigger::Threshold,
            summary: summary.to_string(),
            retained,
            model: ModelIdentity {
                model: "glm-5.1".to_string(),
                model_config: Some("test".to_string()),
                reasoning_effort: None,
            },
            summary_instruction_version: 1,
            estimated_input: EstimatedInput {
                before_tokens: 100,
                after_tokens: 40,
                basis: EstimateBasis::ContentClass,
            },
        }
    }

    /// A retained plain message.
    pub fn message(role: Role, content: &str) -> RetainedRecord {
        RetainedRecord {
            record: RetainedConversationRecord::Message(MessageData {
                role,
                content: content.to_string(),
                id: None,
                status: None,
                timestamp: None,
                images: Vec::new(),
            }),
            pin: None,
            source_task: None,
            origin: None,
            skill_read: None,
        }
    }

    /// A retained `Read`-style exchange: call plus its output.
    pub fn exchange(call_id: &str, output: &str) -> Vec<RetainedRecord> {
        vec![
            RetainedRecord {
                record: RetainedConversationRecord::FunctionCall(FunctionCallData {
                    id: format!("fc-{call_id}"),
                    call_id: call_id.to_string(),
                    name: "Read".to_string(),
                    arguments: "{}".to_string(),
                    arguments_parse_error: None,
                    replay: None,
                    timestamp: None,
                }),
                pin: None,
                source_task: None,
                origin: None,
                skill_read: None,
            },
            RetainedRecord {
                record: RetainedConversationRecord::FunctionCallOutput(FunctionCallOutputData {
                    call_id: call_id.to_string(),
                    output: output.to_string(),
                    replay: None,
                    timestamp: None,
                    images: Vec::new(),
                }),
                pin: None,
                source_task: None,
                origin: None,
                skill_read: None,
            },
        ]
    }

    /// A retained reasoning item.
    pub fn reasoning(id: &str) -> RetainedRecord {
        RetainedRecord {
            record: RetainedConversationRecord::Reasoning(ReasoningData {
                id: id.to_string(),
                summary: None,
                encrypted_content: None,
                content: None,
                timestamp: None,
            }),
            pin: None,
            source_task: None,
            origin: None,
            skill_read: None,
        }
    }

    /// Mark a retained message as actual user input.
    pub fn as_user_input(mut record: RetainedRecord) -> RetainedRecord {
        record.pin = Some(super::PinReason::UserInput);
        record.origin = Some(MessageOrigin::User);
        record
    }

    /// Mark a retained output with observed skill-instruction provenance.
    pub fn as_skill_read(mut record: RetainedRecord, name: &str) -> RetainedRecord {
        record.skill_read = Some(super::SkillRead {
            name: name.to_string(),
            path: PathBuf::from(format!("/skills/{name}/SKILL.md")),
        });
        record.pin = Some(super::PinReason::SkillInstructions {
            name: name.to_string(),
            path: PathBuf::from(format!("/skills/{name}/SKILL.md")),
        });
        record.source_task = Some("550e8400-e29b-41d4-a716-446655440001".to_string());
        record
    }
}
