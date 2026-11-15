use super::*;
use crate::config::blob_store::ContentId;
use crate::config::model::ApiType;
use crate::config::settings::ResolvedImagePolicy;
use crate::types::ImagePart;
use base64::Engine as _;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use std::io::Cursor;
use wiremock::matchers::{method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

fn resolved_model(base_url: &str) -> ResolvedModelConfig {
    ResolvedModelConfig {
        model_config: crate::config::model::ModelConfig {
            supports_images: true,
            model: "test-model".to_string(),
            api_type: ApiType::ChatCompletions,
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
    }
}

fn test_agent(base_url: &str) -> Agent {
    Agent::new(
        resolved_model(base_url),
        &[(Role::System, "test system prompt".to_string())],
    )
    .with_tools(ToolRegistry::empty())
}

fn final_chat_response() -> serde_json::Value {
    serde_json::json!({
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
    })
}

fn png_part(width: u32, height: u32) -> ImagePart {
    let image = DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |x, y| {
        Rgb([
            u8::try_from(x % 256).unwrap(),
            u8::try_from(y % 256).unwrap(),
            u8::try_from((x + y) % 256).unwrap(),
        ])
    }));
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .unwrap();
    ImagePart {
        media_type: "image/png".to_string(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        content_id: None,
    }
}

fn user_message(content: &str, images: Vec<ImagePart>) -> ConversationItem {
    ConversationItem::Message {
        role: Role::User,
        content: content.to_string(),
        id: None,
        status: None,
        timestamp: None,
        images,
    }
}

fn decoded_len(image: &ImagePart) -> usize {
    base64::engine::general_purpose::STANDARD
        .decode(&image.data_base64)
        .unwrap()
        .len()
}

#[derive(Debug)]
struct RequestShape {
    present: Vec<String>,
    absent: Vec<String>,
    markers: Vec<String>,
}

impl Match for RequestShape {
    fn matches(&self, request: &Request) -> bool {
        let body = String::from_utf8_lossy(&request.body);
        self.present.iter().all(|value| body.contains(value))
            && !self.absent.iter().any(|value| body.contains(value))
            && self.markers.iter().all(|marker| body.contains(marker))
    }
}

#[tokio::test]
async fn agent_omits_the_overflowing_image_sends_markers_and_keeps_raw_history() {
    let mock_server = MockServer::start().await;
    let old_kept = png_part(10, 10);
    let newer_kept = png_part(11, 11);
    let omitted = png_part(12, 12);
    let marker = format!(
        "[image omitted: count; image/png, 12x12, {} bytes; the request image budget is exhausted for this run, so crop or downscale the source, or continue from text]",
        decoded_len(&omitted)
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(RequestShape {
            present: vec![old_kept.data_url(), newer_kept.data_url()],
            absent: vec![omitted.data_url()],
            markers: vec![marker],
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(final_chat_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let history = vec![
        user_message("old", vec![old_kept, newer_kept]),
        user_message("new", vec![omitted]),
    ];
    let mut agent = test_agent(&mock_server.uri())
        .with_history(history)
        .unwrap()
        .with_image_policy(&ResolvedImagePolicy {
            max_images: 2,
            max_bytes: 64 * 1024 * 1024,
            max_pixels: 100_000_000,
        });

    assert_eq!(agent.send("go".to_string()).await.unwrap(), "Hello!");
    assert_eq!(
        agent.history_image_count(),
        3,
        "the raw conversation must keep every original image"
    );
}

#[tokio::test]
async fn image_overflow_omits_instead_of_failing_the_request() {
    let mock_server = MockServer::start().await;
    let kept = png_part(10, 10);
    let omitted = png_part(11, 11);
    let marker = format!(
        "[image omitted: count; image/png, 11x11, {} bytes; the request image budget is exhausted for this run, so crop or downscale the source, or continue from text]",
        decoded_len(&omitted)
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(RequestShape {
            present: vec![kept.data_url()],
            absent: vec![omitted.data_url()],
            markers: vec![marker],
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(final_chat_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let history = vec![user_message("current", vec![kept, omitted])];
    let mut agent = test_agent(&mock_server.uri())
        .with_history(history)
        .unwrap()
        .with_image_policy(&ResolvedImagePolicy {
            max_images: 1,
            max_bytes: 64 * 1024 * 1024,
            max_pixels: 100_000_000,
        });

    assert_eq!(agent.send("go".to_string()).await.unwrap(), "Hello!");
    mock_server.verify().await;
    assert_eq!(
        agent.history_image_count(),
        2,
        "the raw conversation must keep the omitted image"
    );
}

#[tokio::test]
async fn corrupt_digest_fails_before_a_provider_request() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_chat_response()))
        .expect(0)
        .mount(&mock_server)
        .await;

    let mut corrupt = png_part(10, 10);
    corrupt.content_id = Some(ContentId::from_bytes(b"not these bytes").to_string());
    let history = vec![user_message("current", vec![corrupt])];
    let mut agent = test_agent(&mock_server.uri())
        .with_history(history)
        .unwrap();

    let error = agent.send("go".to_string()).await.unwrap_err();
    assert!(error.to_string().contains("has content id"), "{error}");
    assert!(error.to_string().contains("its bytes hash to"), "{error}");
    mock_server.verify().await;
}
