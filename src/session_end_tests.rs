use std::time::{Duration, Instant};

use crate::clients::Agent;
use crate::config::hooks::{HookEvent, HookGroup, HookMatcher, LoadedHooks};
use crate::config::model::ApiType;
use crate::config::{HooksLoader, ModelConfig, ResolvedModelConfig};
use crate::hooks::{HookContext, HookRunner, SessionEndReason};
use crate::types::{Role, StreamRecord};
use crate::{CodingAssistant, Interrupted};

#[test]
fn session_end_loads_without_matcher() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks.json");
    std::fs::write(
        &path,
        r#"{"version":1,"hooks":{"SessionEnd":[{"hooks":[{"type":"command","command":"true"}]}]}}"#,
    )
    .unwrap();

    let loaded = HooksLoader::load_from_paths([path.as_path()]).unwrap();

    assert!(!HookEvent::SessionEnd.has_source());
    assert!(loaded.groups.iter().any(|group| {
        group.event == HookEvent::SessionEnd && matches!(group.matcher, HookMatcher::All)
    }));

    std::fs::write(
        &path,
        r#"{"version":1,"hooks":{"SessionEnd":[{"matcher":"*","hooks":[{"type":"command","command":"true"}]}]}}"#,
    )
    .unwrap();
    let error = HooksLoader::load_from_paths([path.as_path()]).unwrap_err();
    assert!(matches!(
        error,
        crate::config::hooks::HooksError::MatcherNotSupported { .. }
    ));
}

#[test]
fn session_end_reason_values_are_stable() {
    assert_eq!(SessionEndReason::Success.as_str(), "success");
    assert_eq!(SessionEndReason::Error.as_str(), "error");
    assert_eq!(SessionEndReason::Interrupted.as_str(), "interrupted");
}

#[tokio::test]
#[cfg(unix)]
async fn session_end_payload_has_common_fields_and_reason() {
    let dir = tempfile::tempdir().unwrap();
    let payload_path = dir.path().join("session-end.json");
    let session_id = uuid::Uuid::new_v4();
    let task_id = uuid::Uuid::new_v4();
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = captured.clone();
    let command = crate::config::hooks::HookCommand {
        command: format!("cat > '{}'", payload_path.display()),
        timeout: Duration::from_secs(2),
        fail_closed: true,
        status_message: None,
        source_path: dir.path().join("hooks.json"),
    };
    let runner = HookRunner::new(
        LoadedHooks {
            groups: vec![HookGroup {
                event: HookEvent::SessionEnd,
                matcher: HookMatcher::All,
                hooks: vec![command],
            }],
        },
        HookContext {
            session_id,
            task_id,
            transcript_path: None,
            hook_event_sink: Some(std::sync::Arc::new(move |record| {
                captured_clone.lock().unwrap().push(record);
                Ok(())
            })),
            cwd: dir.path().to_path_buf(),
            model: "test-model".to_string(),
        },
    );

    runner.session_end(SessionEndReason::Error).await.unwrap();

    let payload: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(payload_path).expect("SessionEnd payload should be captured"),
    )
    .unwrap();
    assert_eq!(payload["version"], 1);
    assert_eq!(payload["session_id"], session_id.to_string());
    assert_eq!(payload["task_id"], task_id.to_string());
    assert!(payload["transcript_path"].is_null());
    assert_eq!(payload["cwd"], dir.path().to_string_lossy().as_ref());
    assert_eq!(payload["hook_event_name"], "SessionEnd");
    assert_eq!(payload["model"], "test-model");
    assert!(payload["timestamp"].is_string());
    assert_eq!(payload["reason"], "error");

    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert!(matches!(
        &captured[0],
        StreamRecord::HookEvent(record) if record.event == "SessionEnd"
    ));
    drop(captured);
}

fn test_model_config() -> ResolvedModelConfig {
    ResolvedModelConfig {
        model_config: ModelConfig {
            model: "test-model".to_string(),
            api_type: ApiType::ChatCompletions,
            base_url: "https://api.example.com".to_string(),
            api_key_env: "TEST_API_KEY".to_string(),
            provider: None,
            provider_headers: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            context_window: None,
            reasoning_effort: None,
            reasoning_summary: None,
            reasoning_max_tokens: None,
            providers: vec![],
        },
        api_key: "test-key".to_string(),
    }
}

fn test_agent() -> Agent {
    Agent::new(
        test_model_config(),
        &[(Role::System, "test system prompt".to_string())],
    )
}

type CapturedHookEvents = std::sync::Arc<std::sync::Mutex<Vec<StreamRecord>>>;

fn hook_runner_for_events(
    commands: Vec<(HookEvent, String, bool)>,
    captured: &CapturedHookEvents,
) -> std::sync::Arc<HookRunner> {
    let cwd = std::env::temp_dir();
    let groups = commands
        .into_iter()
        .map(|(event, command, fail_closed)| HookGroup {
            event,
            matcher: HookMatcher::All,
            hooks: vec![crate::config::hooks::HookCommand {
                command,
                timeout: Duration::from_secs(2),
                fail_closed,
                status_message: None,
                source_path: cwd.join("hooks.json"),
            }],
        })
        .collect();
    let captured_clone = captured.clone();
    std::sync::Arc::new(HookRunner::new(
        LoadedHooks { groups },
        HookContext {
            session_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::new_v4(),
            transcript_path: None,
            hook_event_sink: Some(std::sync::Arc::new(move |record| {
                captured_clone.lock().unwrap().push(record);
                Ok(())
            })),
            cwd,
            model: "test-model".to_string(),
        },
    ))
}

fn session_end_event_count(events: &CapturedHookEvents) -> usize {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|record| {
            matches!(record, StreamRecord::HookEvent(event) if event.event == "SessionEnd")
        })
        .count()
}

#[tokio::test]
#[cfg(unix)]
async fn session_end_emits_once_for_success_and_failed_turns() {
    let cases = [
        ("success", Ok("test response".to_string())),
        ("error", Err(anyhow::anyhow!("test error"))),
    ];

    for (expected_reason, result) in cases {
        let dir = tempfile::tempdir().unwrap();
        let prior_hook_marker = dir.path().join("prior-hook-ran");
        let payload_path = dir.path().join("session-end.json");
        let task_output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let task_output_clone = task_output.clone();
        let mut agent = test_agent().with_streaming_json(move |json| {
            *task_output_clone.lock().unwrap() = json.to_string();
        });
        let prior_event = if expected_reason == "success" {
            HookEvent::Stop
        } else {
            HookEvent::ErrorOccurred
        };
        let hook_events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = hook_runner_for_events(
            vec![
                (
                    prior_event,
                    format!("touch '{}'", prior_hook_marker.display()),
                    false,
                ),
                (
                    HookEvent::SessionEnd,
                    format!(
                        "test -f '{}' && cat > '{}'",
                        prior_hook_marker.display(),
                        payload_path.display()
                    ),
                    false,
                ),
            ],
            &hook_events,
        );

        CodingAssistant::handle_agent_turn_result(&mut agent, Some(&runner), &result, 50)
            .await
            .unwrap();

        assert_eq!(session_end_event_count(&hook_events), 1);
        let payload: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&payload_path).expect("SessionEnd payload should be captured"),
        )
        .unwrap();
        assert_eq!(payload["reason"], expected_reason);
        let task_complete: serde_json::Value =
            serde_json::from_str(&task_output.lock().unwrap()).unwrap();
        assert_eq!(task_complete["type"], "task_complete");
    }
}

#[tokio::test]
#[cfg(unix)]
async fn session_end_emits_once_for_pre_send_failure() {
    let mut agent = test_agent();
    let dir = tempfile::tempdir().unwrap();
    let payload_path = dir.path().join("session-end.json");
    let hook_events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let runner = hook_runner_for_events(
        vec![
            (HookEvent::SessionStart, "exit 2".to_string(), false),
            (
                HookEvent::SessionEnd,
                format!("cat > '{}'", payload_path.display()),
                false,
            ),
        ],
        &hook_events,
    );

    let result = CodingAssistant::execute_agent_turn(
        &mut agent,
        Some(&runner),
        crate::config::hooks::HookSource::SessionStart("startup".to_string()),
        "test prompt",
    )
    .await;
    let Err(_error) = result else {
        panic!("SessionStart exit 2 should fail before send");
    };

    assert_eq!(session_end_event_count(&hook_events), 1);
    let payload: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(payload_path).expect("SessionEnd payload should be captured"),
    )
    .unwrap();
    assert_eq!(payload["reason"], "error");
}

#[tokio::test]
#[cfg(unix)]
async fn prepare_agent_turn_emits_task_start_and_succeeds() {
    for with_hooks in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("prompt-submit-ran");
        let records = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let records_clone = records.clone();
        let mut agent = test_agent().with_streaming_json(move |json| {
            records_clone.lock().unwrap().push(json.to_string());
        });
        let runner = with_hooks.then(|| {
            hook_runner_for_events(
                vec![
                    (HookEvent::SessionStart, "exit 0".to_string(), false),
                    (
                        HookEvent::UserPromptSubmit,
                        format!("touch '{}'", marker.display()),
                        false,
                    ),
                ],
                &std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            )
        });

        CodingAssistant::prepare_agent_turn(
            &mut agent,
            runner.as_ref(),
            &crate::config::hooks::HookSource::SessionStart("startup".to_string()),
            "test prompt",
        )
        .await
        .unwrap();

        assert_eq!(
            marker.exists(),
            with_hooks,
            "UserPromptSubmit hook did not run"
        );
        let records = records.lock().unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|record| record.contains("\"task_start\""))
                .count(),
            1
        );
        // The comment on the task-start emission claims the record opens the
        // stream, so nothing else may precede it.
        let first: serde_json::Value = serde_json::from_str(&records[0]).unwrap();
        assert_eq!(first["type"], "task_start");
        drop(records);
    }
}

#[tokio::test]
#[cfg(unix)]
async fn session_end_dispatches_at_most_once_per_invocation() {
    let dir = tempfile::tempdir().unwrap();
    let payload_path = dir.path().join("session-end.json");
    let hook_events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let runner = hook_runner_for_events(
        vec![(
            HookEvent::SessionEnd,
            format!("cat >> '{}'", payload_path.display()),
            false,
        )],
        &hook_events,
    );

    runner.session_end(SessionEndReason::Success).await.unwrap();
    // The interrupt path can reach `session_end` after the turn's own dispatch
    // already completed, because a signal drops the turn future. It must not
    // run the command a second time.
    runner
        .session_end(SessionEndReason::Interrupted)
        .await
        .unwrap();

    let captured = std::fs::read_to_string(&payload_path).unwrap();
    let payloads: Vec<serde_json::Value> = captured
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(payloads.len(), 1, "SessionEnd ran more than once");
    assert_eq!(payloads[0]["reason"], "success");
    assert_eq!(session_end_event_count(&hook_events), 1);
}

#[tokio::test]
#[cfg(unix)]
async fn handle_interrupt_emits_one_session_end_before_task_complete() {
    let dir = tempfile::tempdir().unwrap();
    let payload_path = dir.path().join("session-end.json");
    let task_output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let task_output_clone = task_output.clone();
    let mut agent = test_agent().with_streaming_json(move |json| {
        *task_output_clone.lock().unwrap() = json.to_string();
    });
    let hook_events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let runner = hook_runner_for_events(
        vec![(
            HookEvent::SessionEnd,
            format!("cat > '{}'", payload_path.display()),
            false,
        )],
        &hook_events,
    );

    let error = CodingAssistant::handle_interrupt(&mut agent, Some(&runner), Instant::now())
        .await
        .unwrap_err();

    assert!(error.downcast_ref::<Interrupted>().is_some());
    assert_eq!(session_end_event_count(&hook_events), 1);
    let payload: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(payload_path).expect("SessionEnd payload should be captured"),
    )
    .unwrap();
    assert_eq!(payload["reason"], "interrupted");
    let task_complete: serde_json::Value =
        serde_json::from_str(&task_output.lock().unwrap()).unwrap();
    assert_eq!(task_complete["type"], "task_complete");
    assert_eq!(task_complete["subtype"], "interrupted");
}

#[tokio::test]
#[cfg(unix)]
async fn session_end_failures_do_not_discard_result() {
    let outcomes = [
        (Ok("test response".to_string()), "success"),
        (Err(anyhow::anyhow!("test error")), "error_during_execution"),
    ];

    for (result, expected_subtype) in outcomes {
        for command in ["exit 1", "printf not-json"] {
            let task_output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let task_output_clone = task_output.clone();
            let mut agent = test_agent().with_streaming_json(move |json| {
                *task_output_clone.lock().unwrap() = json.to_string();
            });
            let hook_events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let runner = hook_runner_for_events(
                vec![(HookEvent::SessionEnd, command.to_string(), true)],
                &hook_events,
            );

            CodingAssistant::handle_agent_turn_result(&mut agent, Some(&runner), &result, 50)
                .await
                .unwrap();

            let task_complete: serde_json::Value =
                serde_json::from_str(&task_output.lock().unwrap()).unwrap();
            assert_eq!(task_complete["type"], "task_complete");
            assert_eq!(task_complete["subtype"], expected_subtype);
        }
    }
}
