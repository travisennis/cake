//! `cake sessions checkpoint` --- append one context-compaction checkpoint.
//!
//! The command is the between-invocation persistence seam for external
//! compaction. The caller owns the selection, the summary, the retained tail,
//! the trigger, and the estimates; Core owns the record envelope, the
//! checkpoint and session identities, the timestamp, the positional boundary,
//! the exclusive session lock, validation, the append, and its sync. It never
//! calls a model and never selects or summarizes records.
//!
//! The payload is a wire protocol: unknown fields fail, and a payload that
//! supplies an envelope field Core owns is rejected before any session byte is
//! touched. The session is locked first, repaired if its previous writer was
//! interrupted, and read through that same handle, so a foreign identity or a
//! derived boundary cannot disagree with the append it describes.

use std::io::{IsTerminal, Read};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cli::CHECK_SESSIONS_CHECKPOINT;
use crate::config::session::{Session, SessionWriter};
use crate::config::{DataDir, SessionLockedError};
use crate::types::compaction::{
    CHECKPOINT_VERSION, CompactionData, CompactionTrigger, EstimatedInput,
    MAX_CHECKPOINT_RECORD_BYTES, ModelIdentity, RetainedRecord, SummaryUsage,
};
use crate::types::session::SessionRecord;

/// Largest payload accepted on stdin.
///
/// The serialized record carries the payload plus its envelope, so a payload
/// at or above the record ceiling can never produce an accepted record. The
/// cap bounds the read instead of buffering an unbounded stream.
const MAX_PAYLOAD_BYTES: u64 = MAX_CHECKPOINT_RECORD_BYTES as u64;

/// Fields the record envelope owns. A payload that supplies one is using a
/// second, conflicting source of identity or position, so it is rejected.
const CORE_OWNED_FIELDS: [&str; 6] = [
    "type",
    "checkpoint_version",
    "checkpoint_id",
    "session_id",
    "timestamp",
    "boundary",
];

/// The caller-supplied checkpoint payload read from stdin.
///
/// This is a protocol, not an internal type: unknown fields fail and every
/// field Core owns is absent. `model` defaults to the session metadata and
/// `task_id` to the session's latest `task_start`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointPayload {
    trigger: CompactionTrigger,
    summary: String,
    retained: Vec<RetainedRecord>,
    estimated_input: EstimatedInput,
    summary_instruction_version: u32,
    #[serde(default)]
    model: Option<ModelIdentity>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    summary_usage: Option<SummaryUsage>,
    #[serde(default)]
    summary_time_ms: Option<u64>,
}

impl CheckpointPayload {
    /// Reject payloads Core can see are unusable before a session is locked.
    ///
    /// Structural checkpoint rules (retained roles, call pairing, provenance,
    /// and the record ceiling) stay in the M1 validator and run on the built
    /// record.
    fn validate(&self) -> Result<(), CheckpointError> {
        if self.summary.trim().is_empty() {
            return Err(CheckpointError::input(
                "checkpoint payload summary must not be empty",
            ));
        }
        let EstimatedInput {
            before_tokens,
            after_tokens,
            ..
        } = self.estimated_input;
        if after_tokens >= before_tokens {
            return Err(CheckpointError::input(format!(
                "checkpoint payload must shrink the context: after_tokens ({after_tokens}) must be \
                 below before_tokens ({before_tokens})"
            )));
        }
        if let Some(model) = &self.model
            && model.model.trim().is_empty()
        {
            return Err(CheckpointError::input(
                "checkpoint payload model.model must not be empty",
            ));
        }
        Ok(())
    }
}

/// Failures `cake sessions checkpoint` reports, each mapped to an exit code.
///
/// Input and identity failures exit `3` and never write. Operational failures
/// exit `1`; a locked session is one, because the input is valid and the same
/// append can succeed once the other invocation finishes.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    /// The invocation's payload, identity, or session reference was rejected.
    #[error("{0}")]
    Input(String),
    /// Another invocation holds the session's append lock.
    #[error(
        "Another cake invocation is currently writing to session {0}. Wait for it to finish or run in a different directory."
    )]
    Locked(String),
    /// The existing session cannot be read or opened for append.
    #[error("cannot append a checkpoint to session {session_id}: {detail}")]
    Unreadable { session_id: String, detail: String },
    /// The validated checkpoint could not be written.
    #[error("failed to append the checkpoint to session {session_id}: {detail}")]
    Append { session_id: String, detail: String },
}

impl CheckpointError {
    /// Build an input failure.
    fn input(message: impl Into<String>) -> Self {
        Self::Input(message.into())
    }

    /// The process exit code that accompanies this failure.
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Input(_) => crate::exit_code::code::INPUT_ERROR,
            Self::Locked(_) | Self::Unreadable { .. } | Self::Append { .. } => {
                crate::exit_code::code::AGENT_ERROR
            },
        }
    }
}

/// What one successful append reports.
#[derive(Debug, Serialize)]
struct CheckpointOutcome {
    session_id: String,
    checkpoint_id: String,
    /// Conversation-item count at the append position.
    boundary: usize,
    before_tokens: u64,
    after_tokens: u64,
    basis: crate::types::compaction::EstimateBasis,
    retained_count: usize,
    session_file: String,
}

/// Run `cake sessions checkpoint <UUID> [--json]`.
///
/// Reads one JSON payload from stdin, appends the checkpoint, and prints the
/// human diagnostic or one diagnostic JSON document.
pub fn checkpoint_command(data_dir: &DataDir, uuid: &str, json: bool) -> anyhow::Result<()> {
    let outcome = run_checkpoint(data_dir, uuid).map_err(|error| {
        crate::cli::report_failure(
            json,
            "sessions checkpoint",
            CHECK_SESSIONS_CHECKPOINT,
            error.into(),
        )
    })?;
    print!("{}", render_outcome(&outcome, json)?);
    Ok(())
}

fn run_checkpoint(data_dir: &DataDir, uuid: &str) -> Result<CheckpointOutcome, CheckpointError> {
    let requested = Uuid::parse_str(uuid)
        .map_err(|_error| CheckpointError::input(format!("invalid session UUID '{uuid}'")))?;
    let input = read_stdin_payload()?;
    append_checkpoint(data_dir, requested, &input)
}

/// Read the payload from a pipe, never from an interactive terminal.
fn read_stdin_payload() -> Result<Vec<u8>, CheckpointError> {
    if std::io::stdin().is_terminal() {
        return Err(CheckpointError::input(
            "no checkpoint payload on stdin; pipe one JSON object into 'cake sessions checkpoint \
             <UUID>'",
        ));
    }

    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_PAYLOAD_BYTES.saturating_add(1))
        .read_to_end(&mut input)
        .map_err(|error| {
            CheckpointError::input(format!(
                "failed to read the checkpoint payload from stdin: {error}"
            ))
        })?;
    if input.len() as u64 > MAX_PAYLOAD_BYTES {
        return Err(CheckpointError::input(format!(
            "checkpoint payload exceeds the {MAX_PAYLOAD_BYTES}-byte limit"
        )));
    }
    if input.iter().all(u8::is_ascii_whitespace) {
        return Err(CheckpointError::input(
            "no checkpoint payload on stdin; pipe one JSON object into 'cake sessions checkpoint \
             <UUID>'",
        ));
    }
    Ok(input)
}

/// Parse and reject the parts of the payload Core can decide without a session.
fn parse_payload(input: &[u8]) -> Result<CheckpointPayload, CheckpointError> {
    let text = std::str::from_utf8(input).map_err(|error| {
        CheckpointError::input(format!("checkpoint payload is not UTF-8: {error}"))
    })?;
    let value: serde_json::Value = serde_json::from_str(text).map_err(|error| {
        CheckpointError::input(format!("checkpoint payload is not JSON: {error}"))
    })?;
    let serde_json::Value::Object(object) = &value else {
        return Err(CheckpointError::input(
            "checkpoint payload must be one JSON object",
        ));
    };
    reject_core_owned_fields(object)?;

    let payload: CheckpointPayload = serde_json::from_value(value).map_err(|error| {
        CheckpointError::input(format!("checkpoint payload is invalid: {error}"))
    })?;
    payload.validate()?;
    Ok(payload)
}

/// Reject a payload that names an envelope field as if it owned it.
fn reject_core_owned_fields(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), CheckpointError> {
    CORE_OWNED_FIELDS
        .iter()
        .find(|field| object.contains_key(**field))
        .map_or(Ok(()), |field| {
            Err(CheckpointError::input(format!(
                "checkpoint payload must not supply the core-owned field '{field}'"
            )))
        })
}

/// Append one validated checkpoint to the locked session.
///
/// The lock is taken before the session is read, the boundary is derived from
/// the append position, and the record is committed through the M1 writer.
fn append_checkpoint(
    data_dir: &DataDir,
    requested: Uuid,
    input: &[u8],
) -> Result<CheckpointOutcome, CheckpointError> {
    let payload = parse_payload(input)?;
    let session_id = requested.to_string();
    let path = data_dir.session_path(requested);
    let file = open_locked_session(data_dir, requested)?;
    let session = read_locked_session(&file, &path, requested)?;
    let boundary = conversation_item_count(&session);

    let checkpoint = CompactionData {
        checkpoint_version: CHECKPOINT_VERSION,
        checkpoint_id: Uuid::new_v4().to_string(),
        session_id: session_id.clone(),
        task_id: resolve_task_id(payload.task_id.as_deref(), &session)?,
        timestamp: chrono::Utc::now(),
        trigger: payload.trigger,
        summary: payload.summary,
        retained: payload.retained,
        model: resolve_model(payload.model, &session)?,
        summary_instruction_version: payload.summary_instruction_version,
        estimated_input: payload.estimated_input,
        summary_usage: payload.summary_usage,
        summary_time_ms: payload.summary_time_ms,
    };

    // Validate before the writer is used, so a rejected checkpoint is an input
    // error with the file untouched. `commit_checkpoint` repeats the same
    // validation under its mutex as the M1 durability contract.
    checkpoint.to_record_bytes().map_err(|error| {
        CheckpointError::input(format!("checkpoint payload was rejected: {error:#}"))
    })?;
    SessionWriter::new(file)
        .commit_checkpoint(&checkpoint)
        .map_err(|error| CheckpointError::Append {
            session_id: session_id.clone(),
            detail: format!("{error:#}"),
        })?;

    Ok(CheckpointOutcome {
        session_id,
        checkpoint_id: checkpoint.checkpoint_id.clone(),
        boundary,
        before_tokens: checkpoint.estimated_input.before_tokens,
        after_tokens: checkpoint.estimated_input.after_tokens,
        basis: checkpoint.estimated_input.basis,
        retained_count: checkpoint.retained.len(),
        session_file: path.display().to_string(),
    })
}

/// Lock the session file, repairing an interrupted final record under the lock.
fn open_locked_session(
    data_dir: &DataDir,
    requested: Uuid,
) -> Result<std::fs::File, CheckpointError> {
    let session_id = requested.to_string();
    data_dir
        .open_session_for_append(requested)
        .map_err(|error| {
            if let Some(locked) = error.downcast_ref::<SessionLockedError>() {
                return CheckpointError::Locked(locked.session_id.clone());
            }
            if has_not_found(&error) {
                return CheckpointError::input(format!("session not found: {session_id}"));
            }
            CheckpointError::Unreadable {
                session_id,
                detail: format!("{error:#}"),
            }
        })
}

/// Read the session through its locked append handle and verify its identity.
fn read_locked_session(
    file: &std::fs::File,
    path: &std::path::Path,
    requested: Uuid,
) -> Result<Session, CheckpointError> {
    let session = Session::load_from_append_handle(file, path).map_err(|error| {
        CheckpointError::Unreadable {
            session_id: requested.to_string(),
            detail: format!("{error:#}"),
        }
    })?;
    if session.id != requested {
        return Err(CheckpointError::input(format!(
            "session file {} contains session {}, not {requested}",
            path.display(),
            session.id
        )));
    }
    Ok(session)
}

/// Whether the error chain reached a missing file.
fn has_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    })
}

/// The conversation-item count at the append position.
///
/// A boundary is a count of provider-visible conversation items, never a record
/// index: metadata records and earlier checkpoints are not conversation, so
/// restart, replay, and fork all reconstruct the same position.
fn conversation_item_count(session: &Session) -> usize {
    session
        .records
        .iter()
        .filter_map(SessionRecord::to_conversation_item)
        .count()
}

/// Use the payload's model identity, or the session's when it was omitted.
fn resolve_model(
    payload: Option<ModelIdentity>,
    session: &Session,
) -> Result<ModelIdentity, CheckpointError> {
    if let Some(model) = payload {
        return Ok(model);
    }
    session
        .model
        .as_deref()
        .filter(|model| !model.trim().is_empty())
        .map_or_else(
            || {
                Err(CheckpointError::input(
                    "checkpoint payload must supply model; the session records no model identity",
                ))
            },
            |model| {
                Ok(ModelIdentity {
                    model: model.to_string(),
                    model_config: session.model_config.clone(),
                    reasoning_effort: None,
                })
            },
        )
}

/// Use the payload's task id, or the session's latest `task_start`.
fn resolve_task_id(payload: Option<&str>, session: &Session) -> Result<String, CheckpointError> {
    if let Some(task_id) = payload {
        return Ok(task_id.to_string());
    }
    session
        .records
        .iter()
        .rev()
        .find_map(|record| match record {
            SessionRecord::TaskStart(data) => Some(data.task_id.clone()),
            _ => None,
        })
        .ok_or_else(|| {
            CheckpointError::input(
                "checkpoint payload must supply task_id; the session has no task_start record",
            )
        })
}

/// Render the append as one diagnostic document or human text.
fn render_outcome(outcome: &CheckpointOutcome, json: bool) -> anyhow::Result<String> {
    if json {
        return crate::cli::DiagnosticDocument::new(
            "sessions checkpoint",
            serde_json::json!({
                "session_id": &outcome.session_id,
                "checkpoint_id": &outcome.checkpoint_id,
                "boundary": outcome.boundary,
                "before_tokens": outcome.before_tokens,
                "after_tokens": outcome.after_tokens,
                "retained_count": outcome.retained_count,
            }),
            Vec::new(),
            serde_json::json!({
                "session_file": &outcome.session_file,
                "basis": outcome.basis,
            }),
        )
        .render();
    }

    Ok(format!(
        "Checkpoint {} appended to session {} at boundary {}.\n\
         Estimated input: {} -> {} tokens ({}); retained records: {}.\n\
         Session file: {}\n",
        outcome.checkpoint_id,
        outcome.session_id,
        outcome.boundary,
        outcome.before_tokens,
        outcome.after_tokens,
        outcome.basis.as_str(),
        outcome.retained_count,
        outcome.session_file,
    ))
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
