//! Tests for `cake sessions checkpoint`.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::config::session::CURRENT_FORMAT_VERSION;
use crate::types::compaction::{EstimateBasis, SummaryUsage};
use crate::types::session::{FunctionCallData, FunctionCallOutputData, MessageData};
use crate::types::{GitState, Role};

const SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const OTHER_ID: &str = "550e8400-e29b-41d4-a716-4466554400ff";

fn make_data_dir() -> (DataDir, TempDir) {
    let tmp = TempDir::new().unwrap();
    let dd = DataDir::new_in_dir(tmp.path());
    std::fs::create_dir_all(dd.sessions_dir()).unwrap();
    (dd, tmp)
}

fn session_uuid() -> Uuid {
    Uuid::parse_str(SESSION_ID).unwrap()
}

fn session_path(dd: &DataDir) -> PathBuf {
    dd.session_path(session_uuid())
}

fn meta_record(session_id: &str, model: Option<&str>) -> SessionRecord {
    SessionRecord::SessionMeta {
        format_version: CURRENT_FORMAT_VERSION,
        session_id: session_id.to_string(),
        timestamp: Utc::now(),
        working_directory: PathBuf::from("/work"),
        model: model.map(ToString::to_string),
        model_config: model.map(|_| "test".to_string()),
        tools: vec!["bash".to_string()],
        cake_version: None,
        system_prompt: None,
        git: GitState::default(),
    }
}

fn task_start(session_id: &str, task_id: &str) -> SessionRecord {
    SessionRecord::TaskStart(crate::types::TaskStartData {
        session_id: session_id.to_string(),
        task_id: task_id.to_string(),
        timestamp: Utc::now(),
        model: None,
        model_config: None,
    })
}

fn message(role: Role, content: &str) -> SessionRecord {
    SessionRecord::Message(MessageData {
        role,
        content: content.to_string(),
        id: None,
        status: None,
        timestamp: None,
        images: Vec::new(),
    })
}

/// A session with five records: header, task boundary, and three conversation
/// items, so the conversation-item boundary is 3.
fn basic_session(path: &Path, model: Option<&str>) -> Vec<u8> {
    write_records(
        path,
        &[
            meta_record(SESSION_ID, model),
            task_start(SESSION_ID, "task-1"),
            message(Role::User, "first question"),
            message(Role::Assistant, "first answer"),
            message(Role::User, "second question"),
        ],
    )
}

fn write_records(path: &Path, records: &[SessionRecord]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    std::fs::write(path, &bytes).unwrap();
    bytes
}

fn payload() -> Value {
    json!({
        "trigger": "manual",
        "summary": "Earlier work is summarized.",
        "retained": [
            {"type": "message", "role": "assistant", "content": "latest answer"}
        ],
        "estimated_input": {
            "before_tokens": 12000,
            "after_tokens": 3000,
            "basis": "measured_usage"
        },
        "summary_instruction_version": 1,
        "summary_usage": {
            "input_tokens": 1457,
            "cached_tokens": 0,
            "output_tokens": 779,
            "reasoning_tokens": 291,
            "summary_visible_tokens": 488,
            "total_tokens": 2236
        },
        "summary_time_ms": 6757
    })
}

fn payload_bytes(payload: &Value) -> Vec<u8> {
    serde_json::to_vec(payload).unwrap()
}

fn append(dd: &DataDir, payload: &Value) -> Result<CheckpointOutcome, CheckpointError> {
    append_checkpoint(dd, session_uuid(), &payload_bytes(payload))
}

#[test]
fn valid_append_preserves_prior_bytes_and_session_identity() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    let prior = basic_session(&path, Some("gpt-test"));

    let outcome = append(&dd, &payload()).unwrap();

    let after = std::fs::read(&path).unwrap();
    assert!(
        after.starts_with(&prior),
        "every byte written before the append must be preserved"
    );
    assert_eq!(
        String::from_utf8_lossy(&after).lines().count(),
        6,
        "the append writes exactly one record"
    );
    assert_eq!(outcome.session_id, SESSION_ID);
    assert_eq!(outcome.boundary, 3);
    assert_eq!(outcome.before_tokens, 12000);
    assert_eq!(outcome.after_tokens, 3000);
    assert_eq!(outcome.basis, EstimateBasis::MeasuredUsage);
    assert_eq!(outcome.retained_count, 1);
    assert_eq!(outcome.session_file, path.display().to_string());

    assert_restored_checkpoint(&path, &outcome);
}

/// The durable half of a valid append: the file loads, the checkpoint becomes
/// the active one, and it names the session, task, and model Core resolved.
fn assert_restored_checkpoint(path: &Path, outcome: &CheckpointOutcome) {
    let loaded = Session::load(path).unwrap();
    assert_eq!(loaded.id, session_uuid());
    assert_eq!(loaded.records.len(), 6);
    assert_eq!(loaded.latest_checkpoint_index(), Some(5));

    let seed = loaded.context_seed();
    assert_eq!(seed.messages.len(), 3);
    assert_eq!(seed.boundary, Some(3));
    let checkpoint = seed.checkpoint.expect("the appended checkpoint restores");
    assert_eq!(checkpoint.checkpoint_id, outcome.checkpoint_id);
    assert_eq!(checkpoint.session_id, SESSION_ID);
    assert_eq!(checkpoint.task_id, "task-1");
    assert_eq!(checkpoint.model.model, "gpt-test");
    assert_eq!(checkpoint.model.model_config.as_deref(), Some("test"));
    assert_eq!(checkpoint.model.reasoning_effort, None);
    assert_eq!(checkpoint.trigger, CompactionTrigger::Manual);
    assert_restored_summary_usage(&checkpoint);
    // The retained tail lives once inside the checkpoint, not as extra records.
    assert_eq!(checkpoint.retained.len(), 1);
}

fn assert_restored_summary_usage(checkpoint: &CompactionData) {
    assert_eq!(
        checkpoint.summary_usage,
        Some(SummaryUsage {
            input_tokens: 1457,
            cached_tokens: 0,
            output_tokens: 779,
            reasoning_tokens: 291,
            summary_visible_tokens: 488,
            total_tokens: 2236,
        })
    );
    assert_eq!(checkpoint.summary_time_ms, Some(6757));
}

#[test]
fn a_second_append_keeps_both_checkpoints_and_selects_the_newest() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    basic_session(&path, Some("gpt-test"));

    let first = append(&dd, &payload()).unwrap();
    // A later turn appends one more conversation item before the next checkpoint.
    {
        let mut file = Session::open_for_append(&path).unwrap();
        Session::append_record(&mut file, &message(Role::Assistant, "second answer")).unwrap();
    }
    let mut later = payload();
    later["summary"] = json!("A later summary.");
    let second = append(&dd, &later).unwrap();

    assert_eq!(first.boundary, 3);
    assert_eq!(second.boundary, 4);

    let loaded = Session::load(&path).unwrap();
    assert_eq!(
        loaded
            .records
            .iter()
            .filter(|record| matches!(record, SessionRecord::Compaction(_)))
            .count(),
        2,
        "checkpoints are append-only history, never replaced"
    );
    let seed = loaded.context_seed();
    assert_eq!(seed.boundary, Some(4));
    let checkpoint = seed.checkpoint.expect("the newest checkpoint restores");
    assert_eq!(checkpoint.checkpoint_id, second.checkpoint_id);
    assert_eq!(checkpoint.summary, "A later summary.");
}

#[test]
fn invalid_payloads_are_input_errors_that_write_nothing() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    basic_session(&path, Some("gpt-test"));

    let mut unknown = payload();
    unknown["unexpected"] = json!(true);

    let mut supplied_checkpoint_id = payload();
    supplied_checkpoint_id["checkpoint_id"] = json!("caller-chosen");

    let mut supplied_boundary = payload();
    supplied_boundary["boundary"] = json!(2);

    let mut supplied_session_id = payload();
    supplied_session_id["session_id"] = json!(OTHER_ID);

    let mut incomplete_pairing = payload();
    incomplete_pairing["retained"] = json!([{
        "type": "function_call",
        "id": "fc-1",
        "call_id": "call-1",
        "name": "Read",
        "arguments": "{}"
    }]);

    let mut no_reduction = payload();
    no_reduction["estimated_input"] =
        json!({"before_tokens": 100, "after_tokens": 100, "basis": "measured_usage"});

    let mut grown = payload();
    grown["estimated_input"] =
        json!({"before_tokens": 100, "after_tokens": 200, "basis": "measured_usage"});

    let mut empty_summary = payload();
    empty_summary["summary"] = json!("   ");

    let mut empty_model = payload();
    empty_model["model"] = json!({"model": "  "});

    let mut invalid_summary_usage = payload();
    invalid_summary_usage["summary_usage"] = json!({"input_tokens": "many"});

    let mut cases: Vec<(&str, Vec<u8>)> = vec![
        ("invalid json", b"{ not json".to_vec()),
        ("not an object", b"[]".to_vec()),
        ("unknown field", payload_bytes(&unknown)),
        (
            "core-owned checkpoint id",
            payload_bytes(&supplied_checkpoint_id),
        ),
        ("core-owned boundary", payload_bytes(&supplied_boundary)),
        ("core-owned session id", payload_bytes(&supplied_session_id)),
        ("incomplete pairing", payload_bytes(&incomplete_pairing)),
        ("no reduction", payload_bytes(&no_reduction)),
        ("grown context", payload_bytes(&grown)),
        ("empty summary", payload_bytes(&empty_summary)),
        ("empty model", payload_bytes(&empty_model)),
        (
            "invalid summary usage",
            payload_bytes(&invalid_summary_usage),
        ),
    ];
    cases.push((
        "retained role",
        payload_bytes(&{
            let mut value = payload();
            value["retained"] = json!([{"type": "message", "role": "system", "content": "no"}]);
            value
        }),
    ));

    for (name, input) in cases {
        let before = std::fs::read(&path).unwrap();
        let error = append_checkpoint(&dd, session_uuid(), &input)
            .expect_err(&format!("{name} must be rejected"));
        assert!(
            matches!(error, CheckpointError::Input(_)),
            "{name}: unexpected error: {error}"
        );
        assert_eq!(
            error.exit_code(),
            crate::exit_code::code::INPUT_ERROR,
            "{name}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "{name} must not write"
        );
    }

    // A rejected payload leaves the session usable for a valid append.
    append(&dd, &payload()).unwrap();
}

#[test]
fn an_oversized_record_is_an_input_error_that_writes_nothing() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    let prior = basic_session(&path, Some("gpt-test"));

    let mut huge = payload();
    huge["summary"] = json!("x".repeat(MAX_CHECKPOINT_RECORD_BYTES));

    let error = append(&dd, &huge).expect_err("an oversized record must be rejected");

    assert!(matches!(error, CheckpointError::Input(_)), "{error}");
    assert_eq!(error.exit_code(), crate::exit_code::code::INPUT_ERROR);
    assert_eq!(std::fs::read(&path).unwrap(), prior);
}

#[test]
fn a_foreign_session_identity_is_an_input_error_that_writes_nothing() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    let prior = write_records(
        &path,
        &[
            meta_record(OTHER_ID, Some("gpt-test")),
            task_start(OTHER_ID, "task-1"),
        ],
    );

    let error = append(&dd, &payload()).expect_err("a foreign session must be rejected");

    assert!(matches!(error, CheckpointError::Input(_)), "{error}");
    assert!(
        error.to_string().contains("contains session"),
        "unexpected message: {error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), prior);
}

#[test]
fn a_missing_session_is_an_input_error() {
    let (dd, _tmp) = make_data_dir();

    let error = append(&dd, &payload()).expect_err("a missing session must be rejected");

    assert!(matches!(error, CheckpointError::Input(_)), "{error}");
    assert!(error.to_string().contains("session not found"), "{error}");
    assert_eq!(error.exit_code(), crate::exit_code::code::INPUT_ERROR);
}

#[test]
fn an_unreadable_session_is_an_operational_failure_that_writes_nothing() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    let prior = write_records(&path, &[meta_record(SESSION_ID, Some("gpt-test"))]);
    let mut corrupt = prior;
    corrupt.extend_from_slice(b"{ not a record\n");
    std::fs::write(&path, &corrupt).unwrap();

    let error = append(&dd, &payload()).expect_err("a corrupt session must be rejected");

    assert!(
        matches!(error, CheckpointError::Unreadable { .. }),
        "{error}"
    );
    assert_eq!(error.exit_code(), crate::exit_code::code::AGENT_ERROR);
    assert_eq!(std::fs::read(&path).unwrap(), corrupt);
}

#[test]
fn a_locked_session_is_an_operational_failure_that_writes_nothing() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    let prior = basic_session(&path, Some("gpt-test"));
    let held = Session::open_for_append(&path).unwrap();

    let error = append(&dd, &payload()).expect_err("a locked session must be rejected");

    assert!(matches!(error, CheckpointError::Locked(_)), "{error}");
    assert_eq!(error.exit_code(), crate::exit_code::code::AGENT_ERROR);
    assert_eq!(std::fs::read(&path).unwrap(), prior);

    // The lock is the only obstacle: once released, the same append succeeds.
    drop(held);
    append(&dd, &payload()).unwrap();
}

#[test]
fn an_interrupted_tail_is_repaired_before_the_checkpoint_is_appended() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    let mut interrupted = basic_session(&path, Some("gpt-test"));
    interrupted.extend_from_slice(br#"{"type":"message","role":"user","content":"interru"#);
    std::fs::write(&path, &interrupted).unwrap();

    let outcome = append(&dd, &payload()).unwrap();

    assert_eq!(outcome.boundary, 3);
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        !String::from_utf8_lossy(&bytes).contains("interru"),
        "the incomplete fragment must be truncated"
    );
    let loaded = Session::load(&path).unwrap();
    assert!(
        matches!(loaded.records.last(), Some(SessionRecord::Compaction(_))),
        "the repaired file must end with the checkpoint"
    );
}

#[test]
fn the_payload_model_defaults_to_the_session_metadata() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    basic_session(&path, Some("gpt-test"));

    append(&dd, &payload()).unwrap();

    let checkpoint = Session::load(&path)
        .unwrap()
        .context_seed()
        .checkpoint
        .unwrap();
    assert_eq!(checkpoint.model.model, "gpt-test");
    assert_eq!(checkpoint.model.model_config.as_deref(), Some("test"));
    assert_eq!(checkpoint.model.reasoning_effort, None);
}

#[test]
fn a_session_without_a_model_requires_a_payload_model() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    basic_session(&path, None);

    let error = append(&dd, &payload()).expect_err("a model identity is required");

    assert!(matches!(error, CheckpointError::Input(_)), "{error}");
    assert!(error.to_string().contains("must supply model"), "{error}");

    let mut supplied = payload();
    supplied["model"] = json!({"model": "other-model", "model_config": "other"});
    append(&dd, &supplied).unwrap();

    let checkpoint = Session::load(&path)
        .unwrap()
        .context_seed()
        .checkpoint
        .unwrap();
    assert_eq!(checkpoint.model.model, "other-model");
    assert_eq!(checkpoint.model.model_config.as_deref(), Some("other"));
}

#[test]
fn the_payload_task_id_defaults_to_the_latest_task_start() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    write_records(
        &path,
        &[
            meta_record(SESSION_ID, Some("gpt-test")),
            task_start(SESSION_ID, "task-1"),
            task_start(SESSION_ID, "task-2"),
            message(Role::User, "question"),
        ],
    );

    append(&dd, &payload()).unwrap();
    let checkpoint = Session::load(&path)
        .unwrap()
        .context_seed()
        .checkpoint
        .unwrap();
    assert_eq!(checkpoint.task_id, "task-2");

    let mut explicit = payload();
    explicit["task_id"] = json!("external-task");
    append(&dd, &explicit).unwrap();
    let checkpoint = Session::load(&path)
        .unwrap()
        .context_seed()
        .checkpoint
        .unwrap();
    assert_eq!(checkpoint.task_id, "external-task");
}

#[test]
fn a_session_without_a_task_start_requires_a_payload_task_id() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    write_records(
        &path,
        &[
            meta_record(SESSION_ID, Some("gpt-test")),
            message(Role::User, "question"),
        ],
    );

    let error = append(&dd, &payload()).expect_err("a task id is required");

    assert!(matches!(error, CheckpointError::Input(_)), "{error}");
    assert!(error.to_string().contains("must supply task_id"), "{error}");
}

#[test]
fn the_json_document_reports_identity_position_estimates_and_file() {
    let outcome = CheckpointOutcome {
        session_id: SESSION_ID.to_string(),
        checkpoint_id: "checkpoint-1".to_string(),
        boundary: 7,
        before_tokens: 1000,
        after_tokens: 250,
        basis: EstimateBasis::MeasuredUsage,
        retained_count: 2,
        session_file: "/data/sessions/session.jsonl".to_string(),
    };

    let rendered = render_outcome(&outcome, true).unwrap();
    let parsed: Value = serde_json::from_str(&rendered).unwrap();

    assert!(rendered.ends_with('\n'));
    assert_eq!(parsed["schema_version"], 1);
    assert_eq!(parsed["command"], "sessions checkpoint");
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["checks"], json!([]));
    assert_eq!(parsed["summary"]["session_id"], SESSION_ID);
    assert_eq!(parsed["summary"]["checkpoint_id"], "checkpoint-1");
    assert_eq!(parsed["summary"]["boundary"], 7);
    assert_eq!(parsed["summary"]["before_tokens"], 1000);
    assert_eq!(parsed["summary"]["after_tokens"], 250);
    assert_eq!(parsed["summary"]["retained_count"], 2);
    assert_eq!(
        parsed["data"]["session_file"],
        "/data/sessions/session.jsonl"
    );
    assert_eq!(parsed["data"]["basis"], "measured_usage");
}

#[test]
fn the_human_output_names_the_checkpoint_boundary_and_estimates() {
    let outcome = CheckpointOutcome {
        session_id: SESSION_ID.to_string(),
        checkpoint_id: "checkpoint-1".to_string(),
        boundary: 7,
        before_tokens: 1000,
        after_tokens: 250,
        basis: EstimateBasis::ContentClass,
        retained_count: 2,
        session_file: "/data/sessions/session.jsonl".to_string(),
    };

    assert_eq!(
        render_outcome(&outcome, false).unwrap(),
        "Checkpoint checkpoint-1 appended to session 550e8400-e29b-41d4-a716-446655440000 at \
         boundary 7.\n\
         Estimated input: 1000 -> 250 tokens (content_class); retained records: 2.\n\
         Session file: /data/sessions/session.jsonl\n"
    );
}

#[test]
fn an_invalid_uuid_is_an_input_error_before_any_payload_is_read() {
    let (dd, _tmp) = make_data_dir();

    let error = run_checkpoint(&dd, "not-a-uuid").expect_err("an invalid UUID must be rejected");

    assert!(matches!(error, CheckpointError::Input(_)), "{error}");
    assert!(
        error.to_string().contains("invalid session UUID"),
        "{error}"
    );
    assert_eq!(error.exit_code(), crate::exit_code::code::INPUT_ERROR);
}

#[test]
fn a_retained_exchange_round_trips_through_the_append() {
    let (dd, _tmp) = make_data_dir();
    let path = session_path(&dd);
    basic_session(&path, Some("gpt-test"));
    let mut with_exchange = payload();
    with_exchange["retained"] = json!([
        {
            "type": "function_call",
            "id": "fc-1",
            "call_id": "call-1",
            "name": "Read",
            "arguments": "{\"path\":\"/work/a.rs\"}"
        },
        {
            "type": "function_call_output",
            "call_id": "call-1",
            "output": "file contents"
        }
    ]);

    let outcome = append(&dd, &with_exchange).unwrap();

    assert_eq!(outcome.retained_count, 2);
    let checkpoint = Session::load(&path)
        .unwrap()
        .context_seed()
        .checkpoint
        .unwrap();
    assert!(matches!(
        &checkpoint.retained[0].record,
        crate::types::compaction::RetainedConversationRecord::FunctionCall(
            FunctionCallData { call_id, name, .. }
        ) if call_id == "call-1" && name == "Read"
    ));
    assert!(matches!(
        &checkpoint.retained[1].record,
        crate::types::compaction::RetainedConversationRecord::FunctionCallOutput(
            FunctionCallOutputData { call_id, output, .. }
        ) if call_id == "call-1" && output == "file contents"
    ));
}
