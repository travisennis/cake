//! Crash-prefix reopen-and-repair coverage and a restore fixed-point check.
//!
//! Two invariants live here, both derived from the durable session file:
//!
//! 1. Every record prefix of a scripted run survives a reopen-and-repair pass,
//!    and a second pass is a no-op. Iterating every prefix derives the crash
//!    boundaries mechanically, so a writer added later inherits coverage
//!    instead of needing a hand-picked case.
//! 2. Restore-derived state (call/output pairing and the last-usage seed) is
//!    recomputed from the file alone and compared with what was loaded. Writer
//!    and reader drift then fails here instead of surfacing later as a broken
//!    pairing or a wrong context size.
//!
//! The agent's accumulated `total_usage` is deliberately not part of the
//! check: restore starts it at zero and it totals only the current
//! invocation's provider attempts, so it is not recomputed from the file.
//!
//! One exception is asserted rather than hidden. A `--fork` seeds its
//! last-usage basis from the source session, but its own file copies no
//! `TurnUsage` (and no source reference), so the fork file is not a fixed
//! point of that seed: a fork that ends before recording its first `TurnUsage`
//! leaves a file a later `--resume <fork-id>` cannot seed from. The test pins
//! that current behavior; the divergence is tracked in #552.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};

use super::*;
use crate::clients::agent_state::ConversationState;
use crate::config::session::CURRENT_FORMAT_VERSION;
use crate::config::{DataDir, Session};
use crate::types::session::{FunctionCallData, FunctionCallOutputData, MessageData};
use crate::types::{GitState, TaskCompleteData, TaskOutcome, TaskStartData, TurnUsageData, Usage};

const SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const TASK_ONE: &str = "550e8400-e29b-41d4-a716-446655440001";
const TASK_TWO: &str = "550e8400-e29b-41d4-a716-446655440002";

/// Deterministic timestamp: `1_753_620_000` is 2025-07-27T12:00:00Z.
fn time(offset_seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_753_620_000 + offset_seconds, 0).expect("valid scripted timestamp")
}

fn session_meta() -> SessionRecord {
    SessionRecord::SessionMeta {
        format_version: CURRENT_FORMAT_VERSION,
        session_id: SESSION_ID.to_string(),
        timestamp: time(0),
        working_directory: PathBuf::from("/work"),
        model: Some("test-model".to_string()),
        model_config: Some("test".to_string()),
        tools: vec![
            "Bash".to_string(),
            "Read".to_string(),
            "Write".to_string(),
            "Edit".to_string(),
        ],
        cake_version: None,
        system_prompt: Some("scripted system prompt".to_string()),
        git: GitState::default(),
    }
}

fn task_start(task_id: &str, offset: i64) -> SessionRecord {
    SessionRecord::TaskStart(TaskStartData {
        session_id: SESSION_ID.to_string(),
        task_id: task_id.to_string(),
        timestamp: time(offset),
    })
}

fn message(role: Role, content: &str, offset: i64) -> SessionRecord {
    SessionRecord::Message(MessageData {
        role,
        content: content.to_string(),
        id: None,
        status: None,
        timestamp: Some(time(offset)),
    })
}

fn function_call(id: &str, call_id: &str, name: &str, offset: i64) -> SessionRecord {
    SessionRecord::FunctionCall(FunctionCallData {
        id: id.to_string(),
        call_id: call_id.to_string(),
        name: name.to_string(),
        arguments: "{}".to_string(),
        arguments_parse_error: None,
        replay: None,
        timestamp: Some(time(offset)),
    })
}

fn function_call_output(call_id: &str, output: &str, offset: i64) -> SessionRecord {
    SessionRecord::FunctionCallOutput(FunctionCallOutputData {
        call_id: call_id.to_string(),
        output: output.to_string(),
        replay: None,
        timestamp: Some(time(offset)),
    })
}

fn turn_usage(task_id: &str, turn: u32, input: u64, output: u64, offset: i64) -> SessionRecord {
    SessionRecord::TurnUsage(TurnUsageData {
        session_id: SESSION_ID.to_string(),
        task_id: task_id.to_string(),
        turn,
        usage: Usage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
            ..Usage::default()
        },
        timestamp: time(offset),
        attempt: None,
        terminal_class: None,
    })
}

fn task_complete(task_id: &str) -> SessionRecord {
    SessionRecord::TaskComplete(TaskCompleteData {
        outcome: TaskOutcome::Success {
            result: Some("done".to_string()),
        },
        duration_ms: 500,
        turn_count: 1,
        tool_call_count: 1,
        session_id: SESSION_ID.to_string(),
        task_id: task_id.to_string(),
        usage: Usage::default(),
        permission_denials: None,
    })
}

/// A two-task run that ends mid-batch: `call-3` recorded its output, `call-4`
/// never did. Every prefix boundary is a distinct crash site.
fn scripted_run_records() -> Vec<SessionRecord> {
    vec![
        session_meta(),
        task_start(TASK_ONE, 1),
        message(Role::User, "list the files", 2),
        message(Role::Assistant, "running a command", 3),
        function_call("fc-1", "call-1", "Bash", 4),
        function_call_output("call-1", "AGENTS.md\nCargo.toml", 5),
        message(Role::Assistant, "Two files.", 6),
        turn_usage(TASK_ONE, 1, 100, 10, 7),
        task_complete(TASK_ONE),
        task_start(TASK_TWO, 9),
        message(Role::User, "now edit two files", 10),
        function_call("fc-2", "call-2", "Read", 11),
        function_call_output("call-2", "contents", 12),
        function_call("fc-3", "call-3", "Write", 13),
        function_call("fc-4", "call-4", "Edit", 14),
        function_call_output("call-3", "wrote file", 15),
        turn_usage(TASK_TWO, 1, 250, 20, 16),
    ]
}

fn serialize_records(records: &[SessionRecord]) -> Vec<String> {
    records
        .iter()
        .map(|record| serde_json::to_string(record).expect("session record should serialize"))
        .collect()
}

/// The call ids left without an output by an interrupted writer.
///
/// Re-derived here rather than calling the repair code, so writer/reader drift
/// shows up as a mismatch instead of both sides moving together. It mirrors
/// `repair_items_for_incomplete_calls`'s pairing rule but tolerates the shapes
/// that function rejects (a duplicate open `call_id`, or an output with no
/// preceding call); the fixtures used here contain neither.
fn unmatched_call_ids(items: &[ConversationItem]) -> Vec<String> {
    let mut open: Vec<String> = Vec::new();
    for item in items {
        match item {
            ConversationItem::FunctionCall { call_id, .. } => open.push(call_id.clone()),
            ConversationItem::FunctionCallOutput { call_id, .. } => {
                if let Some(position) = open.iter().position(|id| id == call_id) {
                    open.remove(position);
                }
            },
            ConversationItem::Message { .. } | ConversationItem::Reasoning { .. } => {},
        }
    }
    open
}

/// The same pairing scan over raw JSON lines, so a loader that dropped or
/// reordered records is caught.
fn unmatched_call_ids_from_raw(lines: &[serde_json::Value]) -> Vec<String> {
    let mut open: Vec<String> = Vec::new();
    for line in lines {
        match line["type"].as_str() {
            Some("function_call") => open.push(
                line["call_id"]
                    .as_str()
                    .expect("function_call has call_id")
                    .to_string(),
            ),
            Some("function_call_output") => {
                let call_id = line["call_id"].as_str().expect("output has call_id");
                if let Some(position) = open.iter().position(|id| id == call_id) {
                    open.remove(position);
                }
            },
            _ => {},
        }
    }
    open
}

fn last_turn_usage_from_raw(lines: &[serde_json::Value]) -> Option<Usage> {
    lines
        .iter()
        .rev()
        .find(|line| line["type"] == "turn_usage")
        .map(|line| {
            serde_json::from_value(line["usage"].clone())
                .expect("turn_usage usage should deserialize")
        })
}

/// Call ids of outputs synthesized by a restore repair, identified by the
/// model-visible closure wording.
fn synthetic_repair_call_ids(items: &[ConversationItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            ConversationItem::FunctionCallOutput {
                call_id, output, ..
            } if output.starts_with("not executed:") => Some(call_id.clone()),
            _ => None,
        })
        .collect()
}

/// Restore `messages` into a fresh conversation and return the non-prompt
/// history plus the outputs synthesized to close unmatched calls.
fn restore(messages: Vec<ConversationItem>) -> (Vec<ConversationItem>, Vec<ConversationItem>) {
    let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
    state
        .with_restored_history(messages)
        .expect("restore should succeed");
    let repairs = state.take_pending_repairs();
    (state.history()[1..].to_vec(), repairs)
}

fn assert_repair_matches_file(agent: &Agent, expected_repairs: &[String], context: &str) {
    assert!(
        unmatched_call_ids(agent.history()).is_empty(),
        "{context}: restored pairing must leave no unmatched call"
    );
    assert_eq!(
        synthetic_repair_call_ids(agent.history()),
        expected_repairs,
        "{context}: restore must synthesize exactly one output per unmatched call"
    );
}

fn session_restore_model_config() -> ResolvedModelConfig {
    ResolvedModelConfig {
        model_config: crate::config::model::ModelConfig {
            model: "test-model".to_string(),
            api_type: crate::config::model::ApiType::ChatCompletions,
            base_url: "https://example.invalid/v1".to_string(),
            api_key_env: "SESSION_RESTORE_TEST_KEY".to_string(),
            provider: None,
            provider_headers: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            context_window: Some(200_000),
            reasoning_effort: None,
            reasoning_summary: None,
            reasoning_max_tokens: None,
            providers: vec![],
        },
        api_key: "test-key".to_string(),
    }
}

fn session_restore_tool_context() -> Arc<ToolContext> {
    Arc::new(ToolContext::new(
        PathBuf::from("/work"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        SandboxPolicy::WorkspaceWrite,
    ))
}

/// The agent `--continue` and `--resume` build, via their shared constructor.
fn restored_agent(session: Session) -> Agent {
    crate::CodingAssistant::restored_client_and_session(
        session,
        session_restore_model_config(),
        "test".to_string(),
        &[(Role::System, "sys".to_string())],
        &HashMap::new(),
        session_restore_tool_context(),
        Vec::new(),
        None,
        uuid::Uuid::nil(),
    )
    .expect("restore should succeed")
    .agent
}

#[test]
fn every_record_prefix_reopens_and_repairs_idempotently() {
    let lines = serialize_records(&scripted_run_records());
    let dir = tempfile::tempdir().expect("tempdir");

    for prefix_len in 1..=lines.len() {
        let prefix = format!("{}\n", lines[..prefix_len].join("\n"));
        let path = dir.path().join(format!("prefix-{prefix_len}.jsonl"));

        // Reopen at a clean boundary: framing repair must not change bytes.
        fs::write(&path, &prefix).expect("write prefix");
        drop(Session::open_for_append(&path).expect("first reopen"));
        let after_first = fs::read(&path).expect("read repaired prefix");
        drop(Session::open_for_append(&path).expect("second reopen"));
        assert_eq!(
            fs::read(&path).expect("read twice-repaired prefix"),
            after_first,
            "prefix {prefix_len}: reopening a complete prefix must be byte-stable"
        );
        assert_eq!(
            after_first,
            prefix.as_bytes(),
            "prefix {prefix_len}: complete records must survive reopen"
        );

        // Reopen with a killed-writer fragment appended: the fragment is
        // truncated back to the boundary, and the second reopen is a no-op.
        fs::write(&path, format!("{prefix}{{")).expect("write fragment");
        drop(Session::open_for_append(&path).expect("reopen truncated fragment"));
        assert_eq!(
            fs::read(&path).expect("read truncated prefix"),
            prefix.as_bytes(),
            "prefix {prefix_len}: an unterminated fragment must be truncated"
        );
        drop(Session::open_for_append(&path).expect("reopen repaired prefix"));
        assert_eq!(
            fs::read(&path).expect("read repaired prefix again"),
            prefix.as_bytes(),
            "prefix {prefix_len}: truncation must be idempotent"
        );

        // Reopen and repair the history the prefix implies.
        let session = Session::load(&path).expect("load prefix");
        let expected_repairs = unmatched_call_ids(&session.messages());
        let (repaired_history, repairs) = restore(session.messages());
        assert_eq!(
            synthetic_repair_call_ids(&repaired_history),
            expected_repairs,
            "prefix {prefix_len}: repair must close exactly the unmatched calls"
        );
        assert_eq!(
            repairs.len(),
            expected_repairs.len(),
            "prefix {prefix_len}: queued repairs must match the synthesized outputs"
        );
        assert!(
            unmatched_call_ids(&repaired_history).is_empty(),
            "prefix {prefix_len}: repaired history must have no unmatched call"
        );

        // The resume flow persists those repairs; restoring again must
        // synthesize nothing and reproduce the same history.
        let (second_history, second_repairs) = restore(repaired_history.clone());
        assert!(
            second_repairs.is_empty(),
            "prefix {prefix_len}: repairing a repaired prefix must be a no-op"
        );
        assert_eq!(
            second_history, repaired_history,
            "prefix {prefix_len}: repaired history must be a fixed point"
        );
    }
}

/// Assert an agent's usage seed matches the value recomputed from the file.
fn assert_usage_seed(actual: Option<Usage>, expected: Usage, context: &str) {
    let actual = actual.unwrap_or_else(|| panic!("{context}: expected a usage seed"));
    assert_eq!(
        (
            actual.input_tokens,
            actual.output_tokens,
            actual.total_tokens
        ),
        (
            expected.input_tokens,
            expected.output_tokens,
            expected.total_tokens
        ),
        "{context}"
    );
}

/// Collect the closure outputs a restore synthesized, append them to the source
/// records the way a resumed run persists repairs, and reload the result.
fn persist_repairs_and_reload(
    restored: &Session,
    agent: &Agent,
    dir: &Path,
    name: &str,
) -> Session {
    let repairs: Vec<SessionRecord> = agent
        .history()
        .iter()
        .filter_map(|item| match item {
            ConversationItem::FunctionCallOutput {
                call_id,
                output,
                timestamp,
            } if output.starts_with("not executed:") => {
                Some(SessionRecord::FunctionCallOutput(FunctionCallOutputData {
                    call_id: call_id.clone(),
                    output: output.clone(),
                    replay: None,
                    timestamp: *timestamp,
                }))
            },
            _ => None,
        })
        .collect();
    let mut repaired_records = restored.records.clone();
    repaired_records.extend(repairs);
    let repaired_lines = serialize_records(&repaired_records);
    let path = dir.join(name);
    fs::write(&path, format!("{}\n", repaired_lines.join("\n"))).expect("write repaired session");
    Session::load(&path).expect("load repaired session")
}

#[test]
fn restore_recomputation_from_the_file_matches_loaded_state() {
    let records = scripted_run_records();
    let lines = serialize_records(&records);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(format!("{SESSION_ID}.jsonl"));
    fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write session");

    // Recompute the derived state from the raw bytes, independently.
    let raw: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).expect("raw line should be JSON"))
        .collect();
    let expected_seed = last_turn_usage_from_raw(&raw).expect("scripted run records turn usage");
    let expected_repairs = unmatched_call_ids_from_raw(&raw);
    assert_eq!(
        expected_repairs,
        vec!["call-4".to_string()],
        "the fixture must end at a crash site"
    );

    let restored = Session::load(&path).expect("load session");
    assert_usage_seed(
        restored.last_turn_usage(),
        expected_seed,
        "loaded usage seed must equal the file's last turn_usage",
    );
    assert_eq!(
        unmatched_call_ids(&restored.messages()),
        expected_repairs,
        "loaded pairing must equal the file's pairing"
    );

    // `--continue` and `--resume` share this constructor.
    let restored_run = restored_agent(restored.clone());
    assert_usage_seed(
        restored_run.last_usage(),
        expected_seed,
        "restore must seed the last-usage basis from the file",
    );
    assert_repair_matches_file(&restored_run, &expected_repairs, "restore");

    // `--fork` seeds its last-usage basis and repairs from the source session,
    // but it starts a new file from filtered seed records.
    let mut forked_run = crate::CodingAssistant::forked_client_and_session(
        &restored,
        session_restore_model_config(),
        "test".to_string(),
        PathBuf::from("/work"),
        &[(Role::System, "sys".to_string())],
        HashMap::new(),
        session_restore_tool_context(),
        Vec::new(),
        None,
        uuid::Uuid::nil(),
    )
    .expect("fork should succeed");
    assert_usage_seed(
        forked_run.agent.last_usage(),
        expected_seed,
        "fork must seed the last-usage basis from the source file",
    );
    assert_repair_matches_file(&forked_run.agent, &expected_repairs, "fork");

    // The seed is in-memory only. Materialize the fork's own file through the
    // real persistence plan: its seed records copy conversation items and
    // `SkillActivated` but no `TurnUsage` (and carry no source reference), so
    // the file is not a fixed point of the seed the agent was given. A fork
    // whose first provider attempt fails thus leaves a file a later
    // `--resume <fork-id>` cannot seed from. Assert the current behavior so a
    // fix (persisting the seed) or a regression is caught here deliberately;
    // the divergence is tracked in #552.
    let data_dir = DataDir::new_in_dir(dir.path());
    let fork_file = crate::cli::execute_persistence_plan(
        forked_run.persistence.take(),
        &data_dir,
        &forked_run.session,
        Vec::new(),
    )
    .expect("persist fork seed file")
    .expect("fork must create a session file");
    drop(fork_file);
    let fork_session =
        Session::load(&data_dir.session_path(forked_run.session.id)).expect("load fork seed file");
    assert!(
        fork_session.last_turn_usage().is_none(),
        "fork seed records must carry no TurnUsage for this gap to be real; \
         update this assertion if the fork starts persisting its seed"
    );

    // Persist the repairs the way a resumed run does, then restore again:
    // through the real constructor the repair is a fixed point.
    let repaired_session =
        persist_repairs_and_reload(&restored, &restored_run, dir.path(), "repaired.jsonl");
    assert_eq!(
        repaired_session.records.len(),
        restored.records.len() + expected_repairs.len(),
        "persisting the repairs must append one output per unmatched call"
    );
    // The repaired file already contains closure outputs, so count history
    // items rather than matching on the closure wording: a second restore must
    // append nothing.
    let repaired_message_count = repaired_session.messages().len();
    let rerun = restored_agent(repaired_session);
    assert_eq!(
        rerun.history().len(),
        1 + repaired_message_count,
        "restoring a repaired file must synthesize no new items"
    );
    assert!(
        unmatched_call_ids(rerun.history()).is_empty(),
        "restoring a repaired file must leave pairing complete"
    );
    assert_usage_seed(
        rerun.last_usage(),
        expected_seed,
        "the usage seed must survive a repaired reopen",
    );
}
