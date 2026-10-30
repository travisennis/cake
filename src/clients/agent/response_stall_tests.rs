//! End-to-end fixture tests for the no-data stall deadline.
//!
//! Each test uses a local TCP server, so no provider credentials or network
//! access are involved. The stall window is shortened so the tests stay fast.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::clients::retry::RetryPolicy;
use crate::clients::tools::ToolRegistry;
use crate::config::OutputSchema;
use crate::config::model::{ApiType, ModelConfig};

fn test_agent(api_type: ApiType, base_url: &str) -> Agent {
    Agent::new(
        ResolvedModelConfig {
            model_config: ModelConfig {
                supports_images: true,
                model: "test-model".to_string(),
                api_type,
                base_url: base_url.to_string(),
                api_key_env: "TEST_API_KEY".to_string(),
                provider: None,
                provider_headers: None,
                temperature: None,
                top_p: None,
                top_k: None,
                max_output_tokens: None,
                context_window: None,
                reasoning_effort: None,
                reasoning_summary: None,
                reasoning_max_tokens: None,
                providers: vec![],
            },
            api_key: "test-key".to_string(),
        },
        &[(Role::System, "test system prompt".to_string())],
    )
    .with_session_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"))
    .with_task_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440001"))
    .with_tools(ToolRegistry::empty())
}

fn push_user_message(agent: &mut Agent) {
    agent.history_mut().push(ConversationItem::Message {
        role: Role::User,
        content: "test".to_string(),
        id: None,
        status: None,
        timestamp: None,
        images: Vec::new(),
    });
}

fn success_chat_body() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "id": "chatcmpl-123",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hello!" },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    }))
    .unwrap()
}

fn success_sse_body() -> String {
    let event = serde_json::json!({
        "type": "response.completed",
        "response": {
            "id": "resp-sse",
            "model": "test-model",
            "status": "completed",
            "output": [{
                "type": "message",
                "id": "msg-1",
                "status": "completed",
                "content": [{ "type": "output_text", "text": "Hello!" }]
            }],
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1,
                "total_tokens": 2
            }
        }
    });
    format!("data: {event}\n\n")
}

/// Read one complete HTTP request (headers plus a content-length body).
async fn read_request(stream: &mut TcpStream) {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let count = stream.read(&mut chunk).await.expect("request readable");
        assert_ne!(count, 0, "connection closed before the request completed");
        buf.extend_from_slice(&chunk[..count]);

        let Some(header_end) = buf.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let content_length = String::from_utf8_lossy(&buf[..header_end])
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if buf.len() >= header_end + 4 + content_length {
            return;
        }
    }
}

async fn write_json_head(stream: &mut TcpStream, content_length: usize) {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
         Content-Length: {content_length}\r\nConnection: keep-alive\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
}

async fn write_error_head(stream: &mut TcpStream, status: &str, content_length: usize) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
         Content-Length: {content_length}\r\nConnection: keep-alive\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
}

async fn write_sse_head(stream: &mut TcpStream) {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
              Connection: keep-alive\r\n\r\n",
        )
        .await
        .unwrap();
    stream.flush().await.unwrap();
}

fn no_retry_policy() -> RetryPolicy {
    RetryPolicy {
        max_retries: 0,
        base_delay: Duration::ZERO,
        max_backoff: Duration::ZERO,
        jitter_divisor: 1,
    }
}

fn one_retry_policy() -> RetryPolicy {
    RetryPolicy {
        max_retries: 2,
        base_delay: Duration::ZERO,
        max_backoff: Duration::ZERO,
        jitter_divisor: 1,
    }
}

fn telemetry_records(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn test_schema() -> Arc<OutputSchema> {
    let raw = serde_json::json!({
        "type": "object",
        "properties": {
            "summary": {"type": "string"}
        },
        "required": ["summary"],
        "additionalProperties": false
    });
    let validator = jsonschema::draft202012::new(&raw).unwrap();
    Arc::new(OutputSchema {
        name: "final_output".to_string(),
        raw,
        validator,
    })
}

fn api_error(error: &anyhow::Error) -> &crate::exit_code::ApiError {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<crate::exit_code::ApiError>())
        .expect("the terminal error should be a typed ApiError")
}

/// Force an RST (linger 0) so the peer sees a reset rather than a clean close.
fn force_rst(stream: TcpStream) {
    if let Ok(std_stream) = stream.into_std() {
        // SAFETY: `std_stream` is a live, owned socket descriptor; setting
        // SO_LINGER=0 makes the closing `drop` emit an RST.
        unsafe {
            let fd = std::os::fd::AsRawFd::as_raw_fd(&std_stream);
            let linger = libc::linger {
                l_onoff: 1,
                l_linger: 0,
            };
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_LINGER,
                std::ptr::addr_of!(linger).cast::<libc::c_void>(),
                libc::socklen_t::try_from(std::mem::size_of::<libc::linger>()).unwrap(),
            );
        }
    }
}

#[tokio::test]
async fn progressing_body_outlives_the_stall_window() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = success_chat_body();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        write_json_head(&mut stream, body.len()).await;
        for chunk in body.chunks(24) {
            stream.write_all(chunk).await.unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
    });

    let mut agent = test_agent(ApiType::ChatCompletions, &format!("http://{address}"))
        .with_stall_window(Duration::from_millis(300));
    push_user_message(&mut agent);

    let turn = agent
        .complete_turn(false)
        .await
        .expect("a body that keeps sending data must not hit the stall deadline");
    assert_eq!(turn.items.len(), 1);
    server.await.unwrap();
}

#[tokio::test]
async fn stall_before_the_first_body_byte_is_terminal_and_names_the_phase() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let accepts = Arc::new(AtomicUsize::new(0));
    let server = tokio::spawn({
        let accepts = Arc::clone(&accepts);
        async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                accepts.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    read_request(&mut stream).await;
                    write_json_head(&mut stream, 128).await;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                });
            }
        }
    });

    let mut agent = test_agent(ApiType::ChatCompletions, &format!("http://{address}"))
        .with_stall_window(Duration::from_millis(150));
    agent.runner.with_retry_policy(one_retry_policy());
    push_user_message(&mut agent);

    let error = agent.complete_turn(false).await.unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("provider response stalled"), "{text}");
    assert!(text.contains("model 'test-model'"), "{text}");
    assert!(text.contains("first byte"), "{text}");
    assert!(text.contains("0 bytes received"), "{text}");
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        1,
        "a stall after a 2xx response must not be retried"
    );
    server.abort();
}

#[tokio::test]
async fn stall_mid_body_reports_the_bytes_received_and_a_preview() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = success_chat_body();
    let accepts = Arc::new(AtomicUsize::new(0));
    let server = tokio::spawn({
        let accepts = Arc::clone(&accepts);
        async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                accepts.fetch_add(1, Ordering::SeqCst);
                let partial = body[..20].to_vec();
                let total = body.len();
                tokio::spawn(async move {
                    read_request(&mut stream).await;
                    write_json_head(&mut stream, total).await;
                    stream.write_all(&partial).await.unwrap();
                    stream.flush().await.unwrap();
                    tokio::time::sleep(Duration::from_secs(30)).await;
                });
            }
        }
    });

    let mut agent = test_agent(ApiType::ChatCompletions, &format!("http://{address}"))
        .with_stall_window(Duration::from_millis(150));
    agent.runner.with_retry_policy(one_retry_policy());
    push_user_message(&mut agent);

    let error = agent.complete_turn(false).await.unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("mid-body"), "{text}");
    assert!(text.contains("(20 bytes received)"), "{text}");
    assert!(text.contains("body preview"), "{text}");
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        1,
        "a stall after a 2xx response must not be retried"
    );
    server.abort();
}

#[tokio::test]
async fn stall_before_response_headers_reports_the_connect_phase() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let mut agent = test_agent(ApiType::ChatCompletions, &format!("http://{address}"))
        .with_stall_window(Duration::from_millis(150));
    agent.runner.with_retry_policy(no_retry_policy());
    push_user_message(&mut agent);

    let error = agent.complete_turn(false).await.unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("provider response stalled"), "{text}");
    assert!(text.contains("connect"), "{text}");
    assert!(text.contains("0 bytes received"), "{text}");
    server.abort();
}

#[tokio::test]
async fn stall_before_response_headers_retries_under_the_existing_policy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = success_chat_body();
    let accepts = Arc::new(AtomicUsize::new(0));
    let server = tokio::spawn({
        let accepts = Arc::clone(&accepts);
        async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let attempt = accepts.fetch_add(1, Ordering::SeqCst);
                let body = body.clone();
                tokio::spawn(async move {
                    read_request(&mut stream).await;
                    if attempt == 0 {
                        // Never answer the first attempt; the client stalls and
                        // retries on a fresh connection.
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        return;
                    }
                    write_json_head(&mut stream, body.len()).await;
                    stream.write_all(&body).await.unwrap();
                    stream.flush().await.unwrap();
                });
            }
        }
    });

    let mut agent = test_agent(ApiType::ChatCompletions, &format!("http://{address}"))
        .with_stall_window(Duration::from_millis(150));
    agent.runner.with_retry_policy(one_retry_policy());
    push_user_message(&mut agent);

    let turn = agent
        .complete_turn(false)
        .await
        .expect("a pre-response stall should retry and then succeed");
    assert_eq!(turn.items.len(), 1);
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn completed_sse_is_kept_when_the_connection_stalls() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = success_sse_body();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        write_sse_head(&mut stream).await;
        stream.write_all(body.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let mut agent = test_agent(ApiType::Responses, &format!("http://{address}"))
        .with_stall_window(Duration::from_millis(150));
    push_user_message(&mut agent);

    let turn = agent
        .complete_turn(false)
        .await
        .expect("a completed SSE stream should survive a trailing stall");
    assert_eq!(turn.provider_request_id.as_deref(), Some("resp-sse"));
    assert_eq!(turn.items.len(), 1);
    server.abort();
}

#[tokio::test]
async fn sse_stall_without_a_terminal_event_reports_mid_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        write_sse_head(&mut stream).await;
        stream
            .write_all(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n")
            .await
            .unwrap();
        stream.flush().await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let mut agent = test_agent(ApiType::Responses, &format!("http://{address}"))
        .with_stall_window(Duration::from_millis(150));
    agent.runner.with_retry_policy(one_retry_policy());
    push_user_message(&mut agent);

    let error = agent.complete_turn(false).await.unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("mid-body"), "{text}");
    assert!(text.contains("body preview"), "{text}");
    assert!(text.contains("response.output_text.delta"), "{text}");
    server.abort();
}

#[tokio::test]
async fn mid_body_stall_keeps_provider_bytes_out_of_telemetry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = success_chat_body();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        write_json_head(&mut stream, body.len()).await;
        stream.write_all(&body[..20]).await.unwrap();
        stream.flush().await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let temp_dir = tempfile::TempDir::new().unwrap();
    let telemetry_path = temp_dir.path().join("telemetry.ndjson");
    let mut agent = test_agent(ApiType::ChatCompletions, &format!("http://{address}"))
        .with_session_telemetry(
            crate::session_telemetry::SessionTelemetryWriter::open(&telemetry_path).unwrap(),
            uuid::uuid!("550e8400-e29b-41d4-a716-446655440002"),
        )
        .with_stall_window(Duration::from_millis(150));
    agent.runner.with_retry_policy(no_retry_policy());
    push_user_message(&mut agent);

    let error = agent.complete_turn(false).await.unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("body preview"), "{text}");

    let attempt = telemetry_records(&telemetry_path)
        .into_iter()
        .find(|record| record["type"] == "api_attempt")
        .expect("a completed attempt record is expected");
    let error = attempt["error"]
        .as_str()
        .expect("a failed attempt records its error");
    assert!(error.contains("provider response stalled"), "{error}");
    assert!(!error.contains("body preview"), "{error}");
    assert!(!error.contains("chatcmpl-123"), "{error}");
    server.abort();
}

#[tokio::test]
async fn stalled_non_2xx_body_keeps_the_stall_diagnostic() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        write_error_head(&mut stream, "401 Unauthorized", 64).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let mut agent = test_agent(ApiType::ChatCompletions, &format!("http://{address}"))
        .with_stall_window(Duration::from_millis(150));
    agent.runner.with_retry_policy(no_retry_policy());
    push_user_message(&mut agent);

    let error = agent.complete_turn(false).await.unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("provider response stalled"), "{text}");
    assert!(text.contains("first byte"), "{text}");
    assert!(text.contains("0 bytes received"), "{text}");
    assert_eq!(crate::exit_code::classify_to_u8(&error), 2);
    let api_error = api_error(&error);
    assert_eq!(api_error.status, 401);
    assert!(matches!(
        api_error.body_read_failure,
        Some(crate::clients::response_stall::BodyReadFailure::Stalled(_))
    ));
    server.abort();
}

#[tokio::test]
async fn non_2xx_body_reset_retains_partial_provider_diagnostics() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let partial = b"{\"error\":\"partial reset\",\"padding\":\"".to_vec();
    let accepts = Arc::new(AtomicUsize::new(0));
    let server = tokio::spawn({
        let accepts = Arc::clone(&accepts);
        async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                accepts.fetch_add(1, Ordering::SeqCst);
                let partial = partial.clone();
                tokio::spawn(async move {
                    read_request(&mut stream).await;
                    let total = partial.len() + 256;
                    let head = format!(
                        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\n\
                         Content-Length: {total}\r\nConnection: keep-alive\r\n\r\n"
                    );
                    stream.write_all(head.as_bytes()).await.unwrap();
                    stream.write_all(&partial).await.unwrap();
                    stream.flush().await.unwrap();
                    // Let the client consume the partial body before the reset.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    force_rst(stream);
                });
            }
        }
    });

    let temp_dir = tempfile::TempDir::new().unwrap();
    let telemetry_path = temp_dir.path().join("telemetry.ndjson");
    let mut agent = test_agent(ApiType::ChatCompletions, &format!("http://{address}"))
        .with_session_telemetry(
            crate::session_telemetry::SessionTelemetryWriter::open(&telemetry_path).unwrap(),
            uuid::uuid!("550e8400-e29b-41d4-a716-446655440004"),
        )
        .with_stall_window(Duration::from_millis(500));
    agent.runner.with_retry_policy(no_retry_policy());
    push_user_message(&mut agent);

    let error = agent.complete_turn(false).await.unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("partial reset"), "{text}");
    assert!(
        [
            "connection reset",
            "broken pipe",
            "unexpected eof",
            "end of file"
        ]
        .iter()
        .any(|cause| text.to_ascii_lowercase().contains(cause)),
        "the terminal error should name the read failure: {text}"
    );
    assert_eq!(crate::exit_code::classify_to_u8(&error), 2);
    let api_error = api_error(&error);
    assert_eq!(api_error.status, 401);
    assert!(matches!(
        api_error.body_read_failure,
        Some(crate::clients::response_stall::BodyReadFailure::Failed(_))
    ));
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        1,
        "a terminal 401 must not be retried after its body read fails"
    );

    let attempt = telemetry_records(&telemetry_path)
        .into_iter()
        .find(|record| record["type"] == "api_attempt")
        .expect("a completed attempt record is expected");
    let telemetry_error = attempt["error"].as_str().unwrap();
    assert!(
        telemetry_error.contains("partial reset"),
        "{telemetry_error}"
    );
    assert!(
        [
            "connection reset",
            "broken pipe",
            "unexpected eof",
            "end of file"
        ]
        .iter()
        .any(|cause| telemetry_error.to_ascii_lowercase().contains(cause)),
        "telemetry should retain the read cause: {telemetry_error}"
    );
    server.abort();
}

#[tokio::test]
async fn stalled_400_does_not_fall_back_from_a_native_constraint() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let accepts = Arc::new(AtomicUsize::new(0));
    let server = tokio::spawn({
        let accepts = Arc::clone(&accepts);
        async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let attempt = accepts.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    read_request(&mut stream).await;
                    if attempt == 0 {
                        let body = success_chat_body();
                        write_json_head(&mut stream, body.len()).await;
                        stream.write_all(&body).await.unwrap();
                        stream.flush().await.unwrap();
                    } else {
                        write_error_head(&mut stream, "400 Bad Request", 128).await;
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                });
            }
        }
    });

    let mut agent = test_agent(ApiType::ChatCompletions, &format!("http://{address}"))
        .with_stall_window(Duration::from_millis(150))
        .with_output_schema(test_schema());
    agent.runner.with_retry_policy(no_retry_policy());

    let error = agent.send("go".to_string()).await.unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("provider response stalled"), "{text}");
    assert!(text.contains("model 'test-model'"), "{text}");
    assert!(text.contains("first byte"), "{text}");
    assert!(text.contains("0 bytes received"), "{text}");
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        2,
        "a stalled 400 must not spend an unconstrained retry"
    );
    let api_error = api_error(&error);
    assert_eq!(api_error.status, 400);
    assert!(matches!(
        api_error.body_read_failure,
        Some(crate::clients::response_stall::BodyReadFailure::Stalled(_))
    ));
    server.abort();
}

#[tokio::test]
async fn stalled_non_2xx_body_keeps_partial_provider_diagnostics() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut partial = b"{\"error\":\"slow auth\",\"padding\":\"".to_vec();
    partial.extend(vec![b'x'; 2_048]);
    partial.extend(b"\"}");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        write_error_head(&mut stream, "401 Unauthorized", 4_096).await;
        stream.write_all(&partial).await.unwrap();
        stream.flush().await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let temp_dir = tempfile::TempDir::new().unwrap();
    let telemetry_path = temp_dir.path().join("telemetry.ndjson");
    let mut agent = test_agent(ApiType::ChatCompletions, &format!("http://{address}"))
        .with_session_telemetry(
            crate::session_telemetry::SessionTelemetryWriter::open(&telemetry_path).unwrap(),
            uuid::uuid!("550e8400-e29b-41d4-a716-446655440003"),
        )
        .with_stall_window(Duration::from_millis(150));
    agent.runner.with_retry_policy(no_retry_policy());
    push_user_message(&mut agent);

    let error = agent.complete_turn(false).await.unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("slow auth"), "{text}");
    assert!(text.contains("provider response stalled"), "{text}");
    assert!(
        text.find("provider response stalled").unwrap() < text.find("slow auth").unwrap(),
        "the stall cause should lead the retained body: {text}"
    );
    assert_eq!(crate::exit_code::classify_to_u8(&error), 2);

    let attempt = telemetry_records(&telemetry_path)
        .into_iter()
        .find(|record| record["type"] == "api_attempt")
        .expect("a completed attempt record is expected");
    let telemetry_error = attempt["error"].as_str().unwrap();
    assert!(
        telemetry_error.starts_with("401 provider response stalled"),
        "the bounded telemetry error must keep the stall cause first: {telemetry_error}"
    );
    assert!(
        !telemetry_error.contains("body preview"),
        "{telemetry_error}"
    );
    server.abort();
}

#[tokio::test]
async fn stalled_stream_keeps_a_terminal_response_failed_event() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = concat!(
        "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp-failed\",",
        "\"error\":{\"message\":\"server exploded\",\"type\":\"invalid_request_error\",",
        "\"code\":\"invalid_request_error\"}}}\n\n",
    );
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        write_sse_head(&mut stream).await;
        stream.write_all(body.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let mut agent = test_agent(ApiType::Responses, &format!("http://{address}"))
        .with_stall_window(Duration::from_millis(150));
    agent.runner.with_retry_policy(no_retry_policy());
    push_user_message(&mut agent);

    let error = agent.complete_turn(false).await.unwrap_err();
    let text = format!("{error:#}");
    assert!(
        text.contains("Responses API stream failed: server exploded"),
        "{text}"
    );
    assert!(!text.contains("provider response stalled"), "{text}");
    server.abort();
}
