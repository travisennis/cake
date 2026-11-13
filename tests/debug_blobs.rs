//! Integration tests for `cake debug blobs`.
//!
//! The command is read-only: these tests exercise text and machine output,
//! stable diagnostics, payload-free findings, and preservation of every file
//! the report inspects.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]
#![allow(
    dead_code,
    reason = "the shared test support module includes settings helpers this binary does not use"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::TestEnv;

const SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.command()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn parse_document(output: &Output) -> Value {
    serde_json::from_str(&stdout(output)).expect("stdout must be one JSON document")
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn sessions_dir(env: &TestEnv) -> PathBuf {
    env.data_dir.join("sessions")
}

fn blob_dir(env: &TestEnv, session_id: &str) -> PathBuf {
    sessions_dir(env).join(format!("{session_id}.blobs"))
}

/// Write the `session_meta` record shared by the fixtures below.
fn session_meta(env: &TestEnv, session_id: &str) -> Value {
    json!({
        "type": "session_meta",
        "format_version": 4,
        "session_id": session_id,
        "timestamp": "2026-01-02T03:00:00Z",
        "working_directory": env.workspace_dir,
        "model": "test-model",
        "model_config": "test",
        "tools": [],
    })
}

/// Write one session file with a metadata record and the supplied records.
fn write_session_records(env: &TestEnv, session_id: &str, records: Vec<Value>) {
    fs::create_dir_all(sessions_dir(env)).expect("failed to create sessions directory");
    let contents = std::iter::once(session_meta(env, session_id))
        .chain(records)
        .map(|record| serde_json::to_string(&record).expect("failed to serialize record"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(
        sessions_dir(env).join(format!("{session_id}.jsonl")),
        contents,
    )
    .expect("failed to write session fixture");
}

/// Write one session whose message references `content_ids`.
fn write_session(env: &TestEnv, session_id: &str, content_ids: &[&str]) {
    let images = content_ids
        .iter()
        .map(|content_id| {
            json!({
                "media_type": "image/png",
                "content_id": content_id,
            })
        })
        .collect::<Vec<_>>();
    let message = json!({
        "type": "message",
        "role": "user",
        "content": "look",
        "timestamp": "2026-01-02T03:00:01Z",
        "images": images,
    });
    write_session_records(env, session_id, vec![message]);
}

/// Write one session whose function-call outputs reference `content_ids`.
fn write_output_session(env: &TestEnv, session_id: &str, content_ids: &[&str]) {
    let records = content_ids
        .iter()
        .enumerate()
        .map(|(index, content_id)| {
            json!({
                "type": "function_call_output",
                "call_id": format!("call-{index}"),
                "output": "",
                "content_id": content_id,
                "timestamp": "2026-01-02T03:00:01Z",
            })
        })
        .collect();
    write_session_records(env, session_id, records);
}

fn write_blob(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().expect("blob parent")).expect("failed to create blob dir");
    fs::write(path, bytes).expect("failed to write blob");
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "one fixture asserts every report and check category"
)]
fn debug_blobs_reports_health_without_deleting_or_printing_payloads() {
    let env = TestEnv::new("cake-debug-blobs-health");
    let valid_bytes = b"PAYLOAD_CANARY";
    let corrupt_bytes = b"CORRUPT_CANARY";
    let valid = digest(valid_bytes);
    let corrupt = digest(b"expected corrupt image");
    let missing = digest(b"missing image");
    let orphan = digest(b"orphan image");
    write_session(&env, SESSION_ID, &[&valid, &corrupt, &missing]);

    let blob_dir = blob_dir(&env, SESSION_ID);
    let valid_path = blob_dir.join(&valid);
    let corrupt_path = blob_dir.join(&corrupt);
    let missing_path = blob_dir.join(&missing);
    let orphan_path = blob_dir.join(&orphan);
    let temporary_path = blob_dir.join(".tmp-interrupted");
    write_blob(&valid_path, valid_bytes);
    write_blob(&corrupt_path, corrupt_bytes);
    write_blob(&orphan_path, b"orphan image");
    write_blob(&temporary_path, b"partial image");

    let text = run(&env, &["debug", "blobs"]);
    assert_eq!(text.status.code(), Some(0), "stderr: {}", stderr(&text));
    assert_eq!(stderr(&text), "");
    let report = stdout(&text);
    assert!(report.contains("Session payload blobs"), "{report}");
    assert!(report.contains(SESSION_ID), "{report}");
    assert!(report.contains("missing references: 1"), "{report}");
    assert!(report.contains("corrupt references: 1"), "{report}");
    assert!(report.contains("unreferenced: 1 blobs"), "{report}");
    assert!(report.contains("temporary files: 1"), "{report}");

    let first = run(&env, &["debug", "blobs", "--json"]);
    let second = run(&env, &["debug", "blobs", "--json"]);
    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    assert_eq!(stderr(&first), "");
    assert_eq!(
        stdout(&first),
        stdout(&second),
        "blob diagnostics must be byte-stable"
    );
    let output = stdout(&first);
    assert!(!output.contains("PAYLOAD_CANARY"), "{output}");
    assert!(!output.contains("CORRUPT_CANARY"), "{output}");
    assert!(!output.contains("data_base64"), "{output}");

    let document = parse_document(&first);
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["command"], "debug blobs");
    assert_eq!(document["status"], "warning");
    assert_eq!(document["summary"]["sessions"], 1);
    assert_eq!(document["summary"]["blobs"], 3);
    assert_eq!(document["summary"]["referenced"], 3);
    assert_eq!(document["summary"]["unreferenced"], 1);
    assert_eq!(document["summary"]["missing_references"], 1);
    assert_eq!(document["summary"]["corrupt_references"], 1);
    assert_eq!(document["summary"]["orphaned_stores"], 0);
    assert_eq!(document["data"]["blobs"]["totals"]["unique_blob_count"], 3);
    assert_eq!(
        document["data"]["blobs"]["totals"]["referenced_bytes"],
        json!(valid_bytes.len() + corrupt_bytes.len())
    );

    let session = &document["data"]["blobs"]["sessions"][0];
    assert_eq!(session["session_id"], SESSION_ID);
    assert_eq!(session["blob_count"], 3);
    assert_eq!(session["referenced_count"], 3);
    assert_eq!(session["unreferenced_count"], 1);
    assert_eq!(session["temporary_file_count"], 1);
    assert_eq!(session["missing_references"][0]["digest"], missing);
    assert_eq!(session["corrupt_references"][0]["digest"], corrupt);
    assert_eq!(session["references_complete"], true);

    let checks = document["checks"].as_array().expect("checks array");
    for id in [
        "blobs.reference_missing",
        "blobs.reference_corrupt",
        "blobs.unreferenced",
        "blobs.temporary_file",
    ] {
        assert!(
            checks.iter().any(|check| check["id"] == id),
            "missing check {id}: {checks:?}"
        );
    }

    assert!(valid_path.exists());
    assert!(corrupt_path.exists());
    assert!(!missing_path.exists());
    assert!(orphan_path.exists());
    assert!(temporary_path.exists());
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "one fixture asserts the text report, JSON document, checks, and payload redaction"
)]
fn debug_blobs_reports_output_references_without_payloads() {
    let env = TestEnv::new("cake-debug-blobs-outputs");
    let valid_bytes = b"OUTPUT_PAYLOAD_CANARY";
    let corrupt_bytes = b"CORRUPT_OUTPUT_CANARY";
    let orphan_bytes = b"ORPHAN_OUTPUT_CANARY";
    let valid = digest(valid_bytes);
    let corrupt = digest(b"expected corrupt output payload");
    let missing = digest(b"missing output payload");
    let orphan = digest(orphan_bytes);
    write_output_session(&env, SESSION_ID, &[&valid, &corrupt, &missing]);

    let dir = blob_dir(&env, SESSION_ID);
    let valid_path = dir.join(&valid);
    let corrupt_path = dir.join(&corrupt);
    let missing_path = dir.join(&missing);
    let orphan_path = dir.join(&orphan);
    write_blob(&valid_path, valid_bytes);
    write_blob(&corrupt_path, corrupt_bytes);
    write_blob(&orphan_path, orphan_bytes);

    let text = run(&env, &["debug", "blobs"]);
    assert_eq!(text.status.code(), Some(0), "stderr: {}", stderr(&text));
    assert_eq!(stderr(&text), "");
    let report = stdout(&text);
    assert!(report.contains("Session payload blobs"), "{report}");
    assert!(report.contains(&missing), "{report}");
    assert!(report.contains(&corrupt), "{report}");
    assert!(!report.contains("referenced image"), "{report}");
    assert!(!report.contains("image references"), "{report}");

    let output = run(&env, &["debug", "blobs", "--json"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let document = parse_document(&output);
    let json_report = stdout(&output);
    for payload in [
        std::str::from_utf8(valid_bytes).expect("valid UTF-8"),
        std::str::from_utf8(corrupt_bytes).expect("corrupt UTF-8"),
        std::str::from_utf8(orphan_bytes).expect("orphan UTF-8"),
    ] {
        assert!(
            !report.contains(payload),
            "payload leaked into text: {payload}"
        );
        assert!(
            !json_report.contains(payload),
            "payload leaked into JSON: {payload}"
        );
    }
    assert_eq!(document["status"], "warning");
    assert_eq!(document["summary"]["referenced"], 3);
    assert_eq!(document["summary"]["unreferenced"], 1);
    assert_eq!(document["summary"]["missing_references"], 1);
    assert_eq!(document["summary"]["corrupt_references"], 1);

    let session = &document["data"]["blobs"]["sessions"][0];
    assert_eq!(session["referenced_count"], 3);
    assert_eq!(session["missing_references"][0]["digest"], missing);
    assert_eq!(session["corrupt_references"][0]["digest"], corrupt);
    assert_eq!(session["unreferenced_count"], 1);
    let checks = document["checks"].as_array().expect("checks array");
    for id in [
        "blobs.reference_missing",
        "blobs.reference_corrupt",
        "blobs.unreferenced",
    ] {
        assert!(
            checks.iter().any(|check| check["id"] == id),
            "missing check {id}: {checks:?}"
        );
    }
    let missing_check = checks
        .iter()
        .find(|check| check["id"] == "blobs.reference_missing")
        .expect("missing reference check");
    assert!(
        missing_check["message"]
            .as_str()
            .expect("missing check message")
            .contains(&format!("referenced payload {missing} is missing at")),
        "{missing_check}"
    );
    let corrupt_check = checks
        .iter()
        .find(|check| check["id"] == "blobs.reference_corrupt")
        .expect("corrupt reference check");
    assert!(
        corrupt_check["message"]
            .as_str()
            .expect("corrupt check message")
            .contains(&format!("referenced payload {corrupt} is corrupt at")),
        "{corrupt_check}"
    );

    assert!(valid_path.exists());
    assert!(corrupt_path.exists());
    assert!(!missing_path.exists());
    assert!(orphan_path.exists());
}

#[test]
fn debug_blobs_reports_an_orphaned_store_and_a_session_without_blobs() {
    let env = TestEnv::new("cake-debug-blobs-orphan");
    write_session(&env, SESSION_ID, &[]);
    let orphan_dir = sessions_dir(&env).join("orphan.blobs");
    let orphan = digest(b"orphan image");
    let orphan_path = orphan_dir.join(&orphan);
    write_blob(&orphan_path, b"orphan image");

    let output = run(&env, &["debug", "blobs", "--json"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    assert_eq!(document["summary"]["sessions"], 1);
    assert_eq!(document["summary"]["orphaned_stores"], 1);
    assert_eq!(document["data"]["blobs"]["sessions"][0]["blob_count"], 0);
    assert_eq!(
        document["data"]["blobs"]["orphaned_stores"][0]["blob_count"],
        1
    );
    let checks = document["checks"].as_array().expect("checks array");
    assert!(
        checks
            .iter()
            .any(|check| check["id"] == "blobs.orphaned_store"),
        "{checks:?}"
    );
    assert!(orphan_path.exists(), "debug blobs must never delete");
}
