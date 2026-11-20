//! Target and repeated-headroom cases for the tail-only growth suite.
//!
//! These drive the production agent loop and policy resolution, not selection
//! in isolation. Every checkpoint must leave the next projection at or below
//! its configured target, and the invariant still compares only main-agent
//! requests within one checkpoint epoch.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use wiremock::{MockServer, ResponseTemplate};

use super::*;
use crate::types::compaction::{CompactionStatus, PinReason};

struct CommittedAttempt {
    before: u64,
    after: u64,
    retired: usize,
    retained: usize,
}

fn recording_read_agent(
    server: &MockServer,
    window: u32,
    policy: CompactionPolicy,
    checkpoints: Arc<AtomicUsize>,
) -> (Agent, Arc<Mutex<Vec<SessionRecord>>>) {
    let persisted = Arc::new(Mutex::new(Vec::<SessionRecord>::new()));
    let sink = Arc::clone(&persisted);
    let agent = test_agent_with_window(ApiType::Responses, &server.uri(), window)
        .with_compaction_policy(policy)
        .unwrap()
        .with_persist_callback(move |record| {
            if matches!(record, SessionRecord::Compaction(_)) {
                checkpoints.fetch_add(1, Ordering::SeqCst);
            }
            sink.lock().unwrap().push(record.clone());
            Ok(())
        })
        .with_tools(crate::clients::tools::read_tool_registry());
    (agent, persisted)
}

fn committed_attempts(records: &[SessionRecord]) -> Vec<CommittedAttempt> {
    let starts: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            SessionRecord::CompactionStart(data) => Some(data),
            _ => None,
        })
        .collect();
    let ends: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            SessionRecord::CompactionEnd(data) => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts.len(),
        ends.len(),
        "every compaction start must have one end"
    );
    starts
        .into_iter()
        .zip(ends)
        .map(|(start, end)| {
            assert_eq!(start.checkpoint_id, end.checkpoint_id);
            assert_eq!(end.status, CompactionStatus::Committed);
            CommittedAttempt {
                before: start.estimated_before_tokens,
                after: end
                    .estimated_after_tokens
                    .expect("a committed end carries the after estimate"),
                retired: end
                    .retired_count
                    .expect("a committed end carries the retired count"),
                retained: end
                    .retained_count
                    .expect("a committed end carries the retained count"),
            }
        })
        .collect()
}

fn function_outputs(records: &[SessionRecord]) -> usize {
    records
        .iter()
        .filter(|record| matches!(record, SessionRecord::FunctionCallOutput(_)))
        .count()
}

fn lifecycle(records: &[SessionRecord]) -> Vec<&'static str> {
    records
        .iter()
        .filter_map(|record| match record {
            SessionRecord::CompactionStart(_) => Some("start"),
            SessionRecord::Compaction(_) => Some("checkpoint"),
            SessionRecord::CompactionEnd(_) => Some("end"),
            _ => None,
        })
        .collect()
}

fn main_epochs(recorded: &[RecordedRequest]) -> Vec<usize> {
    recorded
        .iter()
        .filter(|request| !request.summary)
        .map(|request| request.checkpoint_epoch)
        .collect()
}

fn assert_epochs_track_checkpoints(recorded: &[RecordedRequest], checkpoints: usize) {
    let epochs = main_epochs(recorded);
    assert_eq!(epochs.first(), Some(&0));
    assert_eq!(epochs.last(), Some(&checkpoints));
    for pair in epochs.windows(2) {
        assert!(
            pair[1] == pair[0] || pair[1] == pair[0] + 1,
            "a main request may only advance one checkpoint epoch: {epochs:?}"
        );
    }
}

fn main_requests(recorded: &[RecordedRequest]) -> Vec<&RecordedRequest> {
    recorded.iter().filter(|request| !request.summary).collect()
}

fn summary_requests(recorded: &[RecordedRequest]) -> usize {
    recorded.iter().filter(|request| request.summary).count()
}

/// An absolute trigger replaces the percentage fallback, and the next request
/// targets three quarters of that line. The source is above the absolute line
/// but below the 80% fallback, so the percentage path could not explain it.
#[tokio::test]
async fn absolute_trigger_targets_three_quarters_of_its_line() {
    let server = MockServer::start().await;
    let dir = tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
    let fixture = write_prose_fixture(
        dir.path(),
        "absolute-trigger.txt",
        "ABSOLUTE_TRIGGER_SENTINEL",
        80,
        200,
    );

    let provider = ScriptedProvider::new();
    provider
        .mount_split(
            &server,
            "/responses",
            vec![
                ResponseTemplate::new(200)
                    .set_body_json(responses_tool_call("call-absolute", &fixture)),
                ResponseTemplate::new(200).set_body_json(responses_text("done")),
            ],
            vec![ResponseTemplate::new(200).set_body_json(responses_text(
                "Earlier work read the absolute-trigger fixture.",
            ))],
        )
        .await;

    let policy = CompactionPolicy {
        enabled: true,
        threshold_percent: 80,
        trigger_tokens: Some(4_000),
        retained_tokens: 1_000,
        summary_tokens: 512,
        summary_timeout_secs: 180,
    };
    let (mut agent, persisted) =
        recording_read_agent(&server, 10_000, policy, provider.checkpoint_counter());
    assert_eq!(
        agent.send("read the fixture".to_string()).await.unwrap(),
        "done"
    );

    let recorded = provider.recorded();
    assert_eq!(recorded.len(), 3, "main, summary, main");
    assert_eq!(summary_requests(&recorded), 1);
    assert_eq!(main_requests(&recorded).len(), 2);
    assert_tail_only_growth(&recorded, "input");

    let records = persisted.lock().unwrap().clone();
    let attempts = committed_attempts(&records);
    assert_eq!(attempts.len(), 1);
    assert!(
        attempts[0].before >= 4_000 && attempts[0].before < 8_000,
        "the absolute line must fire before the 80% fallback: {}",
        attempts[0].before
    );
    assert!(
        attempts[0].after <= 3_000,
        "the next request must target three quarters of 4,000: {}",
        attempts[0].after
    );
    assert_eq!(
        attempts[0].retained, 1,
        "only the pinned user input remains"
    );
    assert_eq!(function_outputs(&records), 1);
    assert_eq!(lifecycle(&records), vec!["start", "checkpoint", "end"]);
    assert_epochs_track_checkpoints(&recorded, attempts.len());
}

/// A 100k window with a 10% trigger has a 7.5k outer target below its 20k
/// retained target. Selection must keep retiring every eligible exchange,
/// including the huge last one, until the hard target fits.
#[tokio::test]
async fn ten_percent_window_retires_past_the_retained_target() {
    let server = MockServer::start().await;
    let dir = tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
    let small = [
        write_prose_fixture(dir.path(), "retained-small-0.txt", "SMALL_0", 12, 200),
        write_prose_fixture(dir.path(), "retained-small-1.txt", "SMALL_1", 12, 200),
        write_prose_fixture(dir.path(), "retained-small-2.txt", "SMALL_2", 12, 200),
    ];
    let huge = write_prose_fixture(dir.path(), "retained-huge.txt", "HUGE_SENTINEL", 300, 200);

    let provider = ScriptedProvider::new();
    provider
        .mount_split(
            &server,
            "/responses",
            vec![
                ResponseTemplate::new(200)
                    .set_body_json(responses_tool_call("call-small-0", &small[0])),
                ResponseTemplate::new(200)
                    .set_body_json(responses_tool_call("call-small-1", &small[1])),
                ResponseTemplate::new(200)
                    .set_body_json(responses_tool_call("call-small-2", &small[2])),
                ResponseTemplate::new(200).set_body_json(responses_tool_call("call-huge", &huge)),
                ResponseTemplate::new(200).set_body_json(responses_text("done")),
            ],
            vec![ResponseTemplate::new(200).set_body_json(responses_text(
                "Earlier work read three small fixtures and one huge fixture.",
            ))],
        )
        .await;

    let policy = CompactionPolicy {
        enabled: true,
        threshold_percent: 10,
        trigger_tokens: None,
        retained_tokens: 20_000,
        summary_tokens: 512,
        summary_timeout_secs: 180,
    };
    let (mut agent, persisted) =
        recording_read_agent(&server, 100_000, policy, provider.checkpoint_counter());
    assert_eq!(
        agent.send("read the fixtures".to_string()).await.unwrap(),
        "done"
    );

    let recorded = provider.recorded();
    assert_eq!(recorded.len(), 6, "five main requests and one summary");
    assert_eq!(summary_requests(&recorded), 1);
    assert_eq!(main_requests(&recorded).len(), 5);
    assert_tail_only_growth(&recorded, "input");

    let records = persisted.lock().unwrap().clone();
    let attempts = committed_attempts(&records);
    assert_eq!(attempts.len(), 1);
    assert!(
        attempts[0].before >= 10_000,
        "the 10% trigger must fire: {}",
        attempts[0].before
    );
    assert!(
        attempts[0].after <= 7_500,
        "the outer target is below the retained target: {}",
        attempts[0].after
    );
    assert_eq!(
        attempts[0].retained, 1,
        "every eligible exchange must be exhausted; only the pinned user input remains"
    );
    assert!(
        attempts[0].retired >= 6,
        "all four exchanges must retire: {}",
        attempts[0].retired
    );
    assert_eq!(function_outputs(&records), 4);

    let checkpoint = records
        .iter()
        .find_map(|record| match record {
            SessionRecord::Compaction(data) => Some(data),
            _ => None,
        })
        .expect("one checkpoint");
    assert_eq!(checkpoint.retained.len(), 1);
    assert_eq!(checkpoint.retained[0].pin, Some(PinReason::UserInput));

    let post_checkpoint = main_requests(&recorded)
        .last()
        .expect("post-checkpoint request")
        .body
        .to_string();
    for retired in ["SMALL_0", "SMALL_1", "SMALL_2", "HUGE_SENTINEL"] {
        assert!(
            !post_checkpoint.contains(retired),
            "the post-checkpoint projection must omit retired raw {retired}: {post_checkpoint}"
        );
    }
    assert!(post_checkpoint.contains(crate::types::compaction::HISTORICAL_SUMMARY_MARKER));
    assert_epochs_track_checkpoints(&recorded, attempts.len());
}

/// Twenty successive small tool turns near the threshold cross it repeatedly.
/// Every successful checkpoint must restore the configured headroom, and the
/// post-checkpoint projection must still grow only at its tail.
#[tokio::test]
async fn twenty_small_turns_restore_headroom_at_every_checkpoint() {
    let server = MockServer::start().await;
    let dir = tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
    let fixture = write_prose_fixture(
        dir.path(),
        "successive-turns.txt",
        "SUCCESSIVE_TURN_SENTINEL",
        12,
        200,
    );

    let mut main_responses: Vec<_> = (0..20)
        .map(|turn| {
            ResponseTemplate::new(200).set_body_json(responses_tool_call_without_usage(
                &format!("call-{turn}"),
                &fixture,
            ))
        })
        .collect();
    main_responses.push(ResponseTemplate::new(200).set_body_json(responses_text("done")));
    let summary_responses = (0..20)
        .map(|_| {
            ResponseTemplate::new(200)
                .set_body_json(responses_text("Earlier turns read the successive fixture."))
        })
        .collect();

    let provider = ScriptedProvider::new();
    provider
        .mount_split(&server, "/responses", main_responses, summary_responses)
        .await;

    let (mut agent, persisted) = recording_read_agent(
        &server,
        10_000,
        compaction_policy(),
        provider.checkpoint_counter(),
    );
    assert_eq!(
        agent
            .send("read the fixture twenty times".to_string())
            .await
            .unwrap(),
        "done"
    );

    let recorded = provider.recorded();
    assert_eq!(main_requests(&recorded).len(), 21);
    assert_tail_only_growth(&recorded, "input");
    let records = persisted.lock().unwrap().clone();
    assert_eq!(function_outputs(&records), 20);
    let attempts = committed_attempts(&records);
    assert!(
        attempts.len() >= 3,
        "twenty small turns must cross the line repeatedly: {} checkpoints",
        attempts.len()
    );
    assert_eq!(
        summary_requests(&recorded),
        attempts.len(),
        "each successful checkpoint makes one summary call"
    );
    for attempt in &attempts {
        assert!(
            attempt.before >= 6_000,
            "a checkpoint starts at the trigger line: {}",
            attempt.before
        );
        assert!(
            attempt.after <= 4_500,
            "the checkpoint restores headroom to the three-quarter target: {}",
            attempt.after
        );
        assert!(
            10_000 - attempt.after >= 5_500,
            "the next request must have at least the target headroom: {}",
            attempt.after
        );
        assert!(attempt.retired > 0);
    }
    let expected_lifecycle: Vec<_> = (0..attempts.len())
        .flat_map(|_| ["start", "checkpoint", "end"])
        .collect();
    assert_eq!(lifecycle(&records), expected_lifecycle);
    assert_epochs_track_checkpoints(&recorded, attempts.len());
}
