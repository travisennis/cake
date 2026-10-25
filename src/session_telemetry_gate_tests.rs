use std::sync::Arc;

use serde_json::json;

use super::*;

fn sink_for(path: &std::path::Path) -> GateTelemetrySink {
    let writer = SessionTelemetryWriter::open(path).expect("open sidecar");
    GateTelemetrySink::new(
        Arc::new(SharedSessionTelemetryWriter::new(writer)),
        SessionTelemetryContext {
            session_id: "session".to_string(),
            invocation_id: "invocation".to_string(),
        },
    )
}

fn startup_record() -> GateStartupTelemetry {
    GateStartupTelemetry {
        gate: "builtin-safety".to_string(),
        version: Some("0.1.0".to_string()),
        mode: "enforce".to_string(),
        phases: vec!["pre_tool".to_string()],
        actions: vec!["bash.exec".to_string()],
        outcome: "ready".to_string(),
        failure_class: None,
        latency_ms: 4,
    }
}

fn decision_record() -> GateDecisionTelemetry {
    GateDecisionTelemetry {
        gate: Some("builtin-safety".to_string()),
        version: Some("0.1.0".to_string()),
        mode: "enforce".to_string(),
        phase: "pre_tool".to_string(),
        action: "bash.exec".to_string(),
        decision: Some("warn".to_string()),
        code: Some("fake-warn".to_string()),
        latency_ms: 12,
        overridden: false,
        bypassed: false,
        failed_open: false,
        disabled: false,
        failure_class: None,
        call_id: Some("raw-call-id-secret".to_string()),
    }
}

fn bypass_record() -> GateDecisionTelemetry {
    GateDecisionTelemetry {
        gate: None,
        version: None,
        mode: "enforce".to_string(),
        phase: "pre_tool".to_string(),
        action: "bash.exec".to_string(),
        decision: None,
        code: None,
        latency_ms: 0,
        overridden: false,
        bypassed: true,
        failed_open: false,
        disabled: false,
        failure_class: None,
        call_id: None,
    }
}

fn read_records(path: &std::path::Path) -> Vec<serde_json::Value> {
    let contents = std::fs::read_to_string(path).expect("read sidecar");
    assert!(!contents.contains("raw-call-id-secret"));
    contents
        .lines()
        .map(|line| serde_json::from_str(line).expect("record parses"))
        .collect()
}

#[test]
fn gate_startup_and_decision_records_are_metadata_only() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("telemetry.ndjson");
    let sink = sink_for(&path);
    sink.record_startup(startup_record());
    sink.record_decision(decision_record());
    drop(sink);

    let records = read_records(&path);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["type"], json!("gate_startup"));
    assert_eq!(records[0]["gate"], json!("builtin-safety"));
    assert_eq!(records[0]["version"], json!("0.1.0"));
    assert_eq!(records[0]["phases"], json!(["pre_tool"]));
    assert_eq!(records[0]["actions"], json!(["bash.exec"]));
    assert_eq!(records[0]["failure_class"], serde_json::Value::Null);
    assert_eq!(records[1]["type"], json!("gate_decision"));
    assert_eq!(records[1]["decision"], json!("warn"));
    assert_eq!(records[1]["code"], json!("fake-warn"));
    assert_eq!(
        records[1]["call_id"],
        json!(digest_identifier("raw-call-id-secret"))
    );
    assert_eq!(records[1]["failed_open"], json!(false));
    assert_eq!(records[1]["disabled"], json!(false));
    assert_forbidden_keys(&records[0]);
    assert_forbidden_keys(&records[1]);
}

fn assert_forbidden_keys(record: &serde_json::Value) {
    for key in ["command", "reason", "cwd", "message", "api_key"] {
        assert!(record.get(key).is_none(), "record leaked {key}");
    }
}

#[test]
fn gate_evidence_and_bypass_records_carry_the_digest_and_effect() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("telemetry.ndjson");
    let sink = sink_for(&path);
    sink.record_evidence(GateEvidenceTelemetry {
        gate: "builtin-safety".to_string(),
        call_id: Some("raw-call-id-secret".to_string()),
        kind: "attempt".to_string(),
        data: json!({"model": "judge", "latency_ms": 12}),
    });
    sink.record_decision(bypass_record());
    drop(sink);

    let records = read_records(&path);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["type"], json!("gate_evidence"));
    assert_eq!(records[0]["kind"], json!("attempt"));
    assert_eq!(
        records[0]["call_id"],
        json!(digest_identifier("raw-call-id-secret"))
    );
    assert_eq!(records[1]["gate"], serde_json::Value::Null);
    assert_eq!(records[1]["decision"], serde_json::Value::Null);
    assert_eq!(records[1]["bypassed"], json!(true));
    assert_eq!(records[1]["disabled"], json!(false));
}

#[test]
fn oversized_evidence_is_replaced_with_one_truncation_payload() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("telemetry.ndjson");
    let sink = sink_for(&path);
    sink.record_evidence(GateEvidenceTelemetry {
        gate: "safety".to_string(),
        call_id: None,
        kind: "attempt".to_string(),
        data: json!({"padding": "x".repeat(9_000)}),
    });
    drop(sink);
    let contents = std::fs::read_to_string(&path).expect("read sidecar");
    let record: serde_json::Value =
        serde_json::from_str(contents.lines().next().expect("one record")).expect("parses");
    assert_eq!(record["type"], json!("gate_evidence"));
    assert_eq!(record["kind"], json!("truncated"));
    assert_eq!(record["data"]["dropped_items"], json!(1));
    assert!(record["data"]["dropped_bytes"].as_u64().unwrap() > 8_192);
    assert!(record.get("call_id").is_none());
}
