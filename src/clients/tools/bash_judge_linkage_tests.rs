//! Judge-event linkage to the originating transcript call (issue #404).
//!
//! The session-metrics suite pairs a transcript Bash call with its judge
//! outcome by hashing the transcript's raw `call_id` with SHA-256. These tests
//! pin the producer side of that contract: every judge event carries the
//! digest of the call it judged, and the raw identifier never reaches
//! telemetry.

use sha2::{Digest, Sha256};

use super::*;
use crate::clients::judge::JudgeVerdict;
use crate::clients::tools::ToolContext;

/// The linkage digest computed from the documented rule, independently of the
/// production helper, so the two cannot drift silently.
fn call_id_digest(raw_call_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_call_id.as_bytes());
    hex::encode(hasher.finalize())
}

/// A verdict outcome for `decision` with no allowlist override.
fn verdict_outcome(decision: JudgeDecision, code: Option<&str>) -> JudgeOutcome {
    JudgeOutcome::Verdict {
        verdict: JudgeVerdict {
            decision,
            code: code.map(str::to_string),
            message: "judged".to_string(),
            confidence: None,
        },
        overridden: false,
    }
}

#[test]
fn every_judge_decision_records_the_linked_transcript_call() {
    // Each bounded verdict records one judge event --- on the `Ok` path for
    // `allow` and `warn`, on the error path for a real `block` --- and each
    // carries the digest of the call it judged.
    let cases = [
        (JudgeDecision::Allow, None, "allow"),
        (
            JudgeDecision::Warn,
            Some("rg-replace-footgun"),
            "warn:rg-replace-footgun",
        ),
        (
            JudgeDecision::Block,
            Some("destructive-rm"),
            "block:destructive-rm",
        ),
    ];
    for (decision, code, detail) in cases {
        let events = match judge_preflight_outcome(
            verdict_outcome(decision, code),
            12,
            Some("call-linkage"),
        ) {
            Ok(preflight) => preflight.compensation_events,
            Err(error) => error.compensation_events,
        };
        assert_eq!(events.len(), 1, "{detail} must record one judge event");
        assert_eq!(events[0].detail.as_deref(), Some(detail));
        assert_eq!(
            events[0].call_id.as_deref(),
            Some(call_id_digest("call-linkage").as_str()),
            "{detail} must carry the digest of the call it judged"
        );
    }
}

#[test]
fn bypass_and_fail_closed_record_the_linked_transcript_call() {
    let bypassed =
        judge_preflight_outcome(JudgeOutcome::Bypassed, 0, Some("call-linkage")).unwrap();
    assert_eq!(bypassed.compensation_events.len(), 1);
    assert_eq!(
        bypassed.compensation_events[0].kind,
        CompensationKind::JudgeBypass
    );
    assert_eq!(
        bypassed.compensation_events[0].call_id.as_deref(),
        Some(call_id_digest("call-linkage").as_str())
    );

    let denied = fail_closed_tool_error("timeout", "judge timed out", Some("call-linkage"));
    assert_eq!(denied.compensation_events.len(), 1);
    assert_eq!(
        denied.compensation_events[0].kind,
        CompensationKind::JudgeFailClosed
    );
    assert_eq!(
        denied.compensation_events[0].call_id.as_deref(),
        Some(call_id_digest("call-linkage").as_str())
    );
}

#[tokio::test]
async fn bash_execution_attributes_judge_events_to_the_transcript_call() {
    // The executor hands the transcript call id to the preflight, so a denial
    // and a bypass both come back carrying its digest, and never the raw
    // identifier.
    let dir = tempfile::tempdir().expect("hermetic temp dir for bash test");
    let mut context = ToolContext::from_current_process();
    context.cwd = dir.path().to_path_buf();
    let args = r#"{"command": "echo judge-linkage"}"#;

    // No judge context: the preflight fails closed before any provider call.
    let error = Box::pin(execute_bash_for_call(
        &context,
        args,
        Some("call-linkage".to_string()),
    ))
    .await
    .unwrap_err();
    assert_eq!(error.compensation_events.len(), 1);
    assert_eq!(
        error.compensation_events[0].detail.as_deref(),
        Some("missing_context")
    );
    assert_eq!(
        error.compensation_events[0].call_id.as_deref(),
        Some(call_id_digest("call-linkage").as_str())
    );

    // A bypassed judge runs the command and still records the linkage.
    context.judge = Some(bypassed_judge_context());
    let result = Box::pin(execute_bash_for_call(
        &context,
        args,
        Some("call-linkage".to_string()),
    ))
    .await
    .unwrap();
    let bypasses: Vec<_> = result
        .compensation_events
        .iter()
        .filter(|event| event.kind == CompensationKind::JudgeBypass)
        .collect();
    assert_eq!(
        bypasses.len(),
        1,
        "bypassed call must record one judge_bypass event"
    );
    assert_eq!(
        bypasses[0].call_id.as_deref(),
        Some(call_id_digest("call-linkage").as_str())
    );
    let serialized = serde_json::to_string(&result.compensation_events).unwrap();
    assert!(
        !serialized.contains("call-linkage"),
        "the raw call id must never reach telemetry: {serialized}"
    );
}
