use serde_json::json;

use super::*;

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZQmcAAAAASUVORK5CYII=";

fn result(
    content: Vec<serde_json::Value>,
    structured: Option<serde_json::Value>,
    error: bool,
) -> McpCallResult {
    McpCallResult {
        content,
        structured_content: structured,
        is_error: error,
    }
}

#[test]
fn text_blocks_are_joined() {
    let rendered = render_content(
        &result(
            vec![
                json!({"type": "text", "text": "one"}),
                json!({"type": "text", "text": "two"}),
            ],
            None,
            false,
        ),
        false,
        None,
    );
    assert_eq!(rendered.output, "one\ntwo");
    assert!(rendered.images.is_empty());
    assert!(!rendered.truncated);
}

#[test]
fn structured_content_is_used_when_there_is_no_text() {
    let rendered = render_content(
        &result(vec![], Some(json!({"temperature": 22.5})), false),
        false,
        None,
    );
    assert_eq!(rendered.output, "{\n  \"temperature\": 22.5\n}");
}

#[test]
fn structured_content_does_not_duplicate_text() {
    let rendered = render_content(
        &result(
            vec![json!({"type": "text", "text": "same"})],
            Some(json!({"x": 1})),
            false,
        ),
        false,
        None,
    );
    assert_eq!(rendered.output, "same");
}

#[test]
fn empty_results_are_explicit() {
    let rendered = render_content(&result(vec![], None, false), false, None);
    assert_eq!(rendered.output, "(no output)");
}

#[test]
fn images_are_attached_for_an_image_capable_model() {
    let rendered = render_content(
        &result(
            vec![json!({"type": "image", "data": PNG, "mimeType": "image/png"})],
            None,
            false,
        ),
        true,
        None,
    );
    assert_eq!(rendered.images.len(), 1);
    assert_eq!(rendered.images[0].media_type, "image/png");
    assert_eq!(rendered.images[0].data_base64, PNG);
    assert!(rendered.output.contains("[image: image/png"));
}

#[test]
fn images_are_omitted_for_a_text_only_model() {
    let rendered = render_content(
        &result(
            vec![json!({"type": "image", "data": PNG, "mimeType": "image/png"})],
            None,
            false,
        ),
        false,
        None,
    );
    assert!(rendered.images.is_empty());
    assert!(rendered.output.contains("does not accept images"));
}

#[test]
fn unsupported_image_types_are_omitted() {
    let rendered = render_content(
        &result(
            vec![json!({"type": "image", "data": PNG, "mimeType": "image/svg+xml"})],
            None,
            false,
        ),
        true,
        None,
    );
    assert!(rendered.images.is_empty());
    assert!(rendered.output.contains("unsupported media type"));
}

#[test]
fn image_count_is_bounded() {
    let content = (0..MAX_MCP_IMAGES + 2)
        .map(|_| json!({"type": "image", "data": PNG, "mimeType": "image/png"}))
        .collect();
    let rendered = render_content(&result(content, None, false), true, None);
    assert_eq!(rendered.images.len(), MAX_MCP_IMAGES);
    assert!(rendered.output.contains("additional image content omitted"));
}

#[test]
fn oversized_images_are_omitted() {
    let rendered = render_content(
        &result(
            vec![json!({"type": "image", "data": PNG, "mimeType": "image/png"})],
            None,
            false,
        ),
        true,
        Some(8),
    );
    assert!(rendered.images.is_empty());
    assert!(rendered.output.contains("exceeds the image limit"));
}

#[test]
fn invalid_base64_is_omitted() {
    let rendered = render_content(
        &result(
            vec![json!({"type": "image", "data": "not base64!", "mimeType": "image/png"})],
            None,
            false,
        ),
        true,
        None,
    );
    assert!(rendered.images.is_empty());
    assert!(rendered.output.contains("invalid base64"));
}

#[test]
fn embedded_resources_become_text() {
    let rendered = render_content(
        &result(
            vec![
                json!({
                    "type": "resource",
                    "resource": {"uri": "file:///tmp/a.txt", "text": "body"}
                }),
                json!({
                    "type": "resource_link",
                    "uri": "file:///tmp/b.txt",
                    "name": "b"
                }),
                json!({
                    "type": "audio",
                    "data": "AAAA",
                    "mimeType": "audio/wav"
                }),
            ],
            None,
            false,
        ),
        false,
        None,
    );
    assert!(rendered.output.contains("body"));
    assert!(rendered.output.contains("resource link: b"));
    assert!(rendered.output.contains("audio content: audio/wav"));
}

#[test]
fn long_output_is_truncated_with_an_event() {
    let rendered = render_content(
        &result(
            vec![json!({"type": "text", "text": "a".repeat(MAX_OUTPUT_BYTES + 10)})],
            None,
            false,
        ),
        false,
        None,
    );
    assert!(rendered.truncated);
    assert!(rendered.output.contains("mcp output truncated"));
    assert!(rendered.output.len() <= MAX_OUTPUT_BYTES + 80);
}

#[test]
fn arguments_must_be_an_object() {
    let error = parse_arguments("mcp__s__t", "[1,2]").unwrap_err();
    assert!(error.message.contains("expected a JSON object"));
}

#[test]
fn empty_arguments_become_an_object() {
    assert_eq!(parse_arguments("mcp__s__t", "").unwrap(), json!({}));
    assert_eq!(parse_arguments("mcp__s__t", "  ").unwrap(), json!({}));
}

#[test]
fn result_parse_rejects_a_non_object() {
    assert!(McpCallResult::parse(&json!([])).is_err());
    assert!(McpCallResult::parse(&json!({"content": "wrong"})).is_err());
}

#[test]
fn error_flag_is_read() {
    let parsed = McpCallResult::parse(&json!({
        "content": [{"type": "text", "text": "bad"}],
        "isError": true
    }))
    .unwrap();
    assert!(parsed.is_error);
}
