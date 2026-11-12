//! End-to-end coverage for content-addressed session images.
//!
//! These tests drive the real binary against a mocked image-capable Responses
//! provider. They prove that a tool-produced image reaches the provider, that
//! the session stores only a blob reference, that resume hydrates the bytes,
//! and that a missing or corrupt blob fails before another provider request.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use sha2::{Digest, Sha256};
use support::TestEnv;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_KEY: &str = "SESSION_IMAGES_TEST_KEY";
const INITIAL_PROMPT: &str = "describe the image";
/// The 1x1 red PNG used by the `ReadImage` tool tests.
const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR42mP4z8AAAAMBAQD3A0FDAAAAAElFTkSuQmCC";

fn write_responses_settings(env: &TestEnv, base_url: &str) {
    env.write_global_settings(&format!(
        r#"
[[models]]
name = "test"
model = "glm-5.1"
base_url = "{base_url}"
api_key_env = "{TEST_KEY}"
api_type = "responses"
supports_images = true
"#
    ));
    env.write_project_settings(
        r#"
default_model = "test"
"#,
    );
}

fn tiny_png() -> Vec<u8> {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD
        .decode(TINY_PNG_BASE64)
        .expect("tiny PNG fixture should decode")
}

fn write_image_fixture(env: &TestEnv) -> PathBuf {
    let image_path = env.workspace_dir.join("tiny-red.png");
    fs::write(&image_path, tiny_png()).expect("failed to write image fixture");
    image_path
}

fn image_tool_call_response(image_path: &Path) -> serde_json::Value {
    serde_json::json!({
        "id": "resp-image-tool",
        "output": [
            {
                "type": "function_call",
                "id": "fc-read-image",
                "call_id": "call-read-image",
                "name": "ReadImage",
                "arguments": serde_json::json!({
                    "path": image_path.display().to_string()
                })
                .to_string()
            }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 }
    })
}

fn final_response(text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "resp-final",
        "output": [
            {
                "type": "message",
                "id": "msg-final",
                "status": "completed",
                "content": [{ "type": "output_text", "text": text }]
            }
        ],
        "usage": { "input_tokens": 20, "output_tokens": 5, "total_tokens": 25 }
    })
}

async fn mount_initial_provider(mock_server: &MockServer, image_path: &Path) {
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(image_tool_call_response(image_path)),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(final_response("The image is a tiny red square.")),
        )
        .expect(1)
        .mount(mock_server)
        .await;
}

fn run_cake(env: &TestEnv, args: &[&str]) -> Output {
    env.command()
        .args(args)
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn session_file(env: &TestEnv) -> PathBuf {
    let sessions_dir = env.data_dir.join("sessions");
    let mut files: Vec<_> = fs::read_dir(&sessions_dir)
        .expect("sessions directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    assert_eq!(files.len(), 1, "expected exactly one session file");
    files.pop().expect("one session file")
}

fn session_id_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .expect("session file should have a UUID name")
        .to_string()
}

fn blob_files(env: &TestEnv, session_id: &str) -> Vec<PathBuf> {
    let blob_dir = env
        .data_dir
        .join("sessions")
        .join(format!("{session_id}.blobs"));
    let mut files: Vec<_> = fs::read_dir(&blob_dir)
        .expect("session blob directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files
}

fn session_records(path: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .expect("session file should be readable")
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("session line should parse"))
        .collect()
}

async fn run_initial_image_task(env: &TestEnv, mock_server: &MockServer) -> (String, PathBuf) {
    let image_path = write_image_fixture(env);
    mount_initial_provider(mock_server, &image_path).await;

    let output = run_cake(env, &[INITIAL_PROMPT]);
    assert_success(&output);

    let session_path = session_file(env);
    let session_id = session_id_from_path(&session_path);
    let blobs = blob_files(env, &session_id);
    assert_eq!(blobs.len(), 1, "one image should produce one blob");
    (session_id, blobs[0].clone())
}

#[tokio::test]
async fn read_image_tool_output_is_stored_as_a_blob_reference() {
    let env = TestEnv::new("cake-session-images-round-trip");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    let (session_id, blob_path) = run_initial_image_task(&env, &mock_server).await;

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 2, "one tool turn and one final turn");
    let first_request = String::from_utf8_lossy(&requests[0].body);
    let second_request = String::from_utf8_lossy(&requests[1].body);
    assert!(
        first_request.contains("ReadImage"),
        "an image-capable model should be offered ReadImage: {first_request}"
    );
    assert!(
        !first_request.contains("input_image"),
        "the first request should not contain an image: {first_request}"
    );
    assert!(
        second_request.contains("input_image") && second_request.contains(TINY_PNG_BASE64),
        "the tool result image should reach the provider: {second_request}"
    );

    let session_path = env
        .data_dir
        .join("sessions")
        .join(format!("{session_id}.jsonl"));
    let session_text = fs::read_to_string(&session_path).expect("session file should be readable");
    assert!(
        !session_text.contains(TINY_PNG_BASE64),
        "the session must not store inline image bytes"
    );

    let records = session_records(&session_path);
    let output_record = records
        .iter()
        .find(|record| record["type"] == "function_call_output" && record["images"].is_array())
        .expect("session should contain the tool output image");
    let images = output_record["images"]
        .as_array()
        .expect("images should be an array");
    assert_eq!(images.len(), 1, "one image expected");
    let image = &images[0];
    assert!(
        image.get("data_base64").is_none(),
        "externalized image should omit inline bytes: {image}"
    );
    let content_id = image["content_id"]
        .as_str()
        .expect("externalized image should have a content id");
    assert_eq!(
        content_id,
        hex::encode(Sha256::digest(tiny_png())),
        "the content id should be the image digest"
    );
    assert_eq!(
        blob_path.file_name().and_then(|name| name.to_str()),
        Some(content_id),
        "the blob should be named by its digest"
    );
    assert_eq!(
        fs::read(&blob_path).expect("blob should be readable"),
        tiny_png(),
        "the blob should contain the image bytes"
    );
}

#[tokio::test]
async fn resume_hydrates_blob_images_and_fails_closed_when_the_blob_is_missing_or_corrupt() {
    let env = TestEnv::new("cake-session-images-resume");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    let (session_id, blob_path) = run_initial_image_task(&env, &mock_server).await;
    let blob_dir = blob_path.parent().expect("blob directory");
    let orphan_path = blob_dir.join(hex::encode(Sha256::digest(b"unreferenced image")));
    fs::write(&orphan_path, b"unreferenced image").expect("orphan blob");
    let temp_path = blob_dir.join(".tmp-interrupted");
    fs::write(&temp_path, b"partial image").expect("temporary blob");

    mock_server.reset().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response("I can see it.")))
        .expect(1)
        .mount(&mock_server)
        .await;

    let resumed = run_cake(&env, &["--resume", &session_id, "what did you see?"]);
    assert_success(&resumed);
    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded resume requests");
    assert_eq!(requests.len(), 1, "one resumed provider call expected");
    let request_body = String::from_utf8_lossy(&requests[0].body);
    assert!(
        request_body.contains("input_image") && request_body.contains(TINY_PNG_BASE64),
        "resume should hydrate the image for the provider: {request_body}"
    );
    assert_eq!(
        blob_files(&env, &session_id).len(),
        1,
        "resume must not duplicate the blob"
    );
    assert!(!orphan_path.exists(), "resume should remove an orphan blob");
    assert!(
        !temp_path.exists(),
        "resume should remove a stale temporary blob"
    );
    assert!(blob_path.exists(), "resume must keep the referenced blob");

    mock_server.reset().await;
    fs::remove_file(&blob_path).expect("failed to remove blob");
    let missing = run_cake(&env, &["--resume", &session_id, "try again"]);
    assert!(
        !missing.status.success(),
        "resume should fail when the image blob is missing"
    );
    let missing_stderr = String::from_utf8_lossy(&missing.stderr);
    let content_id = blob_path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("blob should have a digest name");
    assert!(missing_stderr.contains(&session_id), "{missing_stderr}");
    assert!(missing_stderr.contains(content_id), "{missing_stderr}");
    assert!(
        missing_stderr.contains("missing"),
        "the error should name the missing blob: {missing_stderr}"
    );
    assert!(
        mock_server
            .received_requests()
            .await
            .expect("recorded failed resume requests")
            .is_empty(),
        "a missing blob must fail before the provider request"
    );

    fs::write(&blob_path, b"corrupt image bytes").expect("failed to corrupt blob");
    let corrupt = run_cake(&env, &["--resume", &session_id, "try once more"]);
    assert!(
        !corrupt.status.success(),
        "resume should fail when the image blob is corrupt"
    );
    let corrupt_stderr = String::from_utf8_lossy(&corrupt.stderr);
    assert!(corrupt_stderr.contains(&session_id), "{corrupt_stderr}");
    assert!(corrupt_stderr.contains(content_id), "{corrupt_stderr}");
    assert!(
        corrupt_stderr.contains("digest mismatch"),
        "the error should name the digest mismatch: {corrupt_stderr}"
    );
    assert!(
        mock_server
            .received_requests()
            .await
            .expect("recorded corrupt resume requests")
            .is_empty(),
        "a corrupt blob must fail before the provider request"
    );
}
