use super::*;
use std::fs;
use tempfile::TempDir;

// =========================================================================
// Multiple Edits Tests
// =========================================================================

#[test]
fn invalid_edit_arguments_report_expected_shape() {
    let err = parse_edit_args(r#"{"path":"src/main.rs","edits":[{"new_text":"replacement"}]}"#)
        .unwrap_err();

    assert!(
        err.contains("Invalid edit arguments"),
        "Error should identify invalid edit arguments: {err}"
    );
    assert!(
        err.contains("missing field `old_text`"),
        "Error should identify the missing field: {err}"
    );
    assert!(
        err.contains("Expected shape"),
        "Error should include a corrective shape: {err}"
    );
    assert!(
        err.contains(r#""path":"file.txt""#),
        "Expected shape should include top-level path: {err}"
    );
    assert!(
        err.contains(r#""old_text":"exact text to replace""#),
        "Expected shape should include per-edit old_text: {err}"
    );
    assert!(
        err.contains(r#""new_text":"replacement text""#),
        "Expected shape should include per-edit new_text: {err}"
    );
}

#[test]
fn invalid_edit_arguments_reject_unknown_field_names() {
    let err = parse_edit_args(
        r#"{"path":"src/main.rs","edits":[{"old_string":"old","new_text":"new"}]}"#,
    )
    .unwrap_err();

    assert!(
        err.contains("unknown field `old_string`"),
        "Error should reject the malformed field name: {err}"
    );
    assert!(
        err.contains("Expected shape"),
        "Error should include a corrective shape: {err}"
    );
}

// =========================================================================
// JSON Parse Error Formatting Tests
// =========================================================================

#[test]
fn error_message_includes_context_window() {
    let err = parse_edit_args(r#"{"path":"src/main.rs","edits":[{"new_text":"replacement"}]}"#)
        .unwrap_err();

    assert!(
        err.contains("Context:"),
        "Error should include context section: {err}"
    );
    // The context window centers on the failure position (column 57).
    // "replacement" or "new_text" should be visible in that window.
    assert!(
        err.contains("new_text"),
        "Context should show part of the payload around failure: {err}"
    );
    assert!(
        err.contains('^'),
        "Context should include a caret marker: {err}"
    );
}

#[test]
fn error_message_missing_field_path_hint() {
    // Provide edits but omit the top-level `path` field.
    let err = parse_edit_args(r#"{"edits":[{"old_text":"hello","new_text":"hi"}]}"#).unwrap_err();

    assert!(
        err.contains("missing field `path`"),
        "Error should identify missing path field: {err}"
    );
    assert!(err.contains("Hint:"), "Error should include a hint: {err}");
    assert!(
        err.contains("`path` field is required"),
        "Hint should mention the path field: {err}"
    );
}

#[test]
fn error_hint_control_char() {
    // Test the hint logic directly via format_json_parse_error.
    // Use a payload with a raw control byte inside a string.
    let corrupted = b"{\"a\": \"hello\x01world\"}";
    let payload = std::str::from_utf8(corrupted).unwrap();
    let err = serde_json::from_str::<serde_json::Value>(payload).unwrap_err();
    let msg =
        crate::clients::tools::format_json_parse_error(payload, &err, "edit", r#"{"a":"..."}"#);

    assert!(
        msg.contains("control character"),
        "Error should mention control character: {msg}"
    );
    assert!(msg.contains("Hint:"), "Error should include a hint: {msg}");
    assert!(
        msg.contains("raw control character"),
        "Hint should mention raw control characters: {msg}"
    );
}

#[test]
fn error_hint_trailing_characters() {
    // Test the hint logic directly.
    // Use a payload with trailing characters after a complete JSON object.
    // We use a streaming deserializer to control the error we get.
    let payload = r#"{"a":1}trailing"#;
    // serde_json::from_str::<Value> fails with "trailing characters" for this
    let err = serde_json::from_str::<serde_json::Value>(payload).unwrap_err();
    let msg = crate::clients::tools::format_json_parse_error(payload, &err, "edit", r#"{"a":1}"#);

    assert!(
        msg.contains("trailing characters") || msg.contains("trailing data"),
        "Error should mention trailing content: {msg}"
    );
    assert!(msg.contains("Hint:"), "Error should include a hint: {msg}");
    assert!(
        msg.contains("Content continued after the closing brace"),
        "Hint should mention content after closing brace: {msg}"
    );
}

#[test]
fn error_message_large_payload_is_bounded() {
    // Build a huge payload that fails JSON parse with an invalid escape.
    let content = "x".repeat(5000);
    // \\x is not a valid JSON escape, so serde will fail after repair
    let payload = format!(
        "{{\"path\":\"x\",\"edits\":[{{\"old_text\":\"{content}\\x\",\"new_text\":\"\"}}]}}",
    );
    let err = parse_edit_args(&payload).unwrap_err();

    // The error message should be well under the payload size
    assert!(
        err.len() < 3000,
        "Error message should be bounded, got {} bytes: {err:.80}...",
        err.len()
    );
    assert!(
        err.contains("Expected shape"),
        "Bounded error should still include the expected shape: {err:.80}..."
    );
    assert!(
        err.contains("Hint:"),
        "Bounded error should still include a hint: {err:.80}..."
    );
}

#[test]
fn edit_schema_enforces_required_argument_shape() {
    let tool = edit_tool();

    let params = tool.parameters;
    assert_eq!(params["required"], serde_json::json!(["path", "edits"]));
    assert_eq!(params["additionalProperties"], serde_json::json!(false));
    assert_eq!(
        params["properties"]["edits"]["items"]["required"],
        serde_json::json!(["old_text", "new_text"])
    );
    assert_eq!(
        params["properties"]["edits"]["items"]["additionalProperties"],
        serde_json::json!(false)
    );
}

#[test]
fn multiple_edits_in_single_call() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world\nGoodbye earth\n").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "Hello", "new_text": "Hi" },
            { "old_text": "Goodbye", "new_text": "Bye" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args).unwrap();
    assert!(result.output.contains("Applied 2 edits"));

    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "Hi world\nBye earth\n");
}

#[test]
fn error_on_overlapping_edits() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "Hello", "new_text": "Hi" },
            { "old_text": "lo wor", "new_text": "xx" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("overlap"),
        "Error should mention overlap: {err}"
    );
}

#[test]
fn error_on_too_many_edits() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world").unwrap();

    let edits: Vec<_> = (0..25)
        .map(|i| serde_json::json!({ "old_text": format!("text{i}"), "new_text": format!("new{i}") }))
        .collect();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": edits
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("Too many edits"),
        "Error should mention too many edits: {err}"
    );
    assert!(
        err.contains("Maximum 20"),
        "Error should mention max: {err}"
    );
}

#[test]
fn error_with_specific_edit_number() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "Hello", "new_text": "Hi" },
            { "old_text": "notfound", "new_text": "replacement" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("Edit 2 of 2"),
        "Error should mention edit number and total: {err}"
    );
    assert!(
        err.contains("could not find"),
        "Error should mention not found: {err}"
    );
}

#[test]
fn error_on_multiple_matches_in_single_edit() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world\nGoodbye world").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "world", "new_text": "universe" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("Edit 1 of 1"),
        "Error should mention edit number and total: {err}"
    );
    assert!(
        err.contains("matches 2 locations"),
        "Error should mention multiple matches: {err}"
    );
    assert!(
        err.contains("Candidate match contexts"),
        "Error should include candidate contexts: {err}"
    );
    assert!(
        err.contains("Match 1 at line 1") && err.contains("Match 2 at line 2"),
        "Error should identify candidate line numbers: {err}"
    );
    assert!(
        err.contains(">     1 | Hello world") && err.contains(">     2 | Goodbye world"),
        "Error should include line-numbered matching snippets: {err}"
    );
}

#[test]
fn ambiguous_match_contexts_are_capped() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    let original = "target 1\ntarget 2\ntarget 3\ntarget 4\ntarget 5\ntarget 6\n";
    fs::write(&file_path, original).unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "target", "new_text": "replacement" }
        ]
    })
    .to_string();

    let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
    assert!(
        err.contains("matches 6 locations"),
        "Error should report the full match count: {err}"
    );
    assert!(
        err.contains("Showing first 5 of 6 matches."),
        "Error should explain the candidate cap: {err}"
    );
    assert!(
        err.contains("Match 5 at line 5") && !err.contains("Match 6 at line 6"),
        "Error should include only the capped candidate list: {err}"
    );

    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, original, "File should be unchanged after failure");
}

#[test]
fn error_on_no_edits_provided() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": []
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("No edits provided"));
}

#[test]
fn single_noop_edit_is_skipped_without_changing_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    let original = "Hello world";
    fs::write(&file_path, original).unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "world", "new_text": "world" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args).unwrap();
    assert!(
        result.output.contains("Skipped 1 no-op edit"),
        "Result should report the skipped no-op: {}",
        result.output
    );
    assert!(
        result.output.contains("No changes made"),
        "Result should state that no changes were made: {}",
        result.output
    );
    assert_eq!(fs::read_to_string(&file_path).unwrap(), original);
}

#[test]
fn mixed_noop_and_meaningful_batch_applies_meaningful_edit() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "alpha\nbeta\ngamma\n").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "alpha", "new_text": "ALPHA" },
            { "old_text": "beta", "new_text": "beta" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args).unwrap();
    assert!(
        result.output.contains("Applied 1 edit"),
        "Result should report the meaningful edit: {}",
        result.output
    );
    assert!(
        result.output.contains("Skipped 1 no-op edit"),
        "Result should report the skipped no-op: {}",
        result.output
    );

    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "ALPHA\nbeta\ngamma\n");
}

#[test]
fn mixed_noop_and_failed_meaningful_batch_leaves_file_unchanged() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    let original = "alpha\nbeta\ngamma\n";
    fs::write(&file_path, original).unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "alpha", "new_text": "ALPHA" },
            { "old_text": "beta", "new_text": "beta" },
            { "old_text": "delta", "new_text": "DELTA" }
        ]
    })
    .to_string();

    let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
    assert!(
        err.contains("Edit 3 of 3"),
        "Error should identify the failing meaningful edit: {err}"
    );
    assert!(
        err.contains("Edit 2 of 3: old_text and new_text were identical."),
        "Error should report the skipped no-op: {err}"
    );
    assert!(
        err.contains("No edits were applied"),
        "Error should state that meaningful edits were rolled back: {err}"
    );

    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, original, "File should be unchanged after failure");
}

// =========================================================================
// Line Ending Tests
// =========================================================================

#[test]
fn preserves_lf_line_endings() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world\nGoodbye earth\n").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "world", "new_text": "universe" }
        ]
    })
    .to_string();

    let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

    let content = fs::read_to_string(&file_path).unwrap();
    assert!(content.contains("Hello universe\n"));
    assert!(!content.contains("\r\n"));
}

#[test]
fn lf_only_content_uses_identity_offset_mapping() {
    // LF-only content must not allocate a per-byte offset table.
    // The offset mapping is implicitly the identity, represented as None.
    let lf_content = "hello\nworld\n";
    let normalized = normalize_crlf_line_endings(lf_content);
    assert!(
        normalized.original_offsets.is_none(),
        "LF-only content should not allocate a per-byte offset table"
    );
    // Verify byte-level identity: every normalized offset maps to itself.
    for i in 0..lf_content.len() {
        assert_eq!(
            normalized.original_offset(i).unwrap(),
            i,
            "normalized offset {i} should map to original offset {i} for LF-only content"
        );
    }
    // The mapping works past the end for the end-of-string boundary.
    assert_eq!(
        normalized.original_offset(lf_content.len()).unwrap(),
        lf_content.len()
    );
}

#[test]
fn crlf_content_uses_explicit_offset_table() {
    // CRLF content must allocate a per-byte offset table.
    let crlf_content = "hello\r\nworld\r\n";
    let normalized = normalize_crlf_line_endings(crlf_content);
    let offsets = normalized
        .original_offsets
        .as_ref()
        .expect("CRLF content should have a per-byte offset table");
    // The normalized content is shorter: each \r\n pair becomes one \n.
    assert_eq!(normalized.content.len(), crlf_content.len() - 2);
    assert_eq!(offsets.len(), normalized.content.len());
    // The original \r\n pairs should be mapped: \n at normalized index 5 maps to
    // original index 5 (the \r), and so on.
    assert_eq!(normalized.original_offset(5).unwrap(), 5); // \r at original[5]
}

#[test]
fn preserves_crlf_line_endings() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world\r\nGoodbye earth\r\n").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "world", "new_text": "universe" }
        ]
    })
    .to_string();

    let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

    let content = String::from_utf8(fs::read(&file_path).unwrap()).unwrap();
    assert!(content.contains("Hello universe\r\n"));
    assert!(content.contains("\r\n"));
}

#[test]
fn does_not_double_encode_crlf_replacement_text() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, b"before\r\ntarget\r\nafter\r\n").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "target", "new_text": "first\r\nsecond\nthird" }
        ]
    })
    .to_string();

    let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

    let content = fs::read(&file_path).unwrap();
    assert_eq!(content, b"before\r\nfirst\r\nsecond\r\nthird\r\nafter\r\n");
}

#[test]
fn preserves_unrelated_bare_cr_bytes() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, b"header\runrelated\r\nHello world\r\n").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "Hello world\n", "new_text": "Hello universe\n" }
        ]
    })
    .to_string();

    let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

    let content = fs::read(&file_path).unwrap();
    assert_eq!(content, b"header\runrelated\r\nHello universe\r\n");
}

#[test]
fn preserves_unedited_mixed_line_endings() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, b"lf only\ncrlf only\r\nreplace me\r\n").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "replace me\n", "new_text": "replaced\n" }
        ]
    })
    .to_string();

    let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

    let content = fs::read(&file_path).unwrap();
    assert_eq!(content, b"lf only\ncrlf only\r\nreplaced\r\n");
}

// =========================================================================
// BOM Tests
// =========================================================================

#[test]
fn handles_utf8_bom() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    let bom: &[u8] = b"\xEF\xBB\xBF";
    let content_with_bom: Vec<u8> = [bom, b"Hello world"].concat();
    fs::write(&file_path, &content_with_bom).unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "world", "new_text": "universe" }
        ]
    })
    .to_string();

    let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

    let content = fs::read(&file_path).unwrap();
    assert!(
        content.starts_with(b"\xEF\xBB\xBF"),
        "BOM should be preserved"
    );
    let content_str = String::from_utf8(content).unwrap();
    assert!(content_str.contains("Hello universe"));
}

#[test]
fn no_bom_stays_no_bom() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "world", "new_text": "universe" }
        ]
    })
    .to_string();

    let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

    let content = fs::read(&file_path).unwrap();
    assert!(
        !content.starts_with(b"\xEF\xBB\xBF"),
        "No BOM should be added"
    );
}

// =========================================================================
// Legacy Tests (from original implementation)
// =========================================================================

#[test]
fn edit_single_occurrence() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world\nGoodbye earth").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "world", "new_text": "universe" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args).unwrap();
    assert!(result.output.contains("Applied 1 edit"));

    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "Hello universe\nGoodbye earth");
}

#[test]
fn error_when_old_text_not_found() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "notfound", "new_text": "replacement" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("could not find"));
}

#[test]
fn delete_text_with_empty_new_text() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": " world", "new_text": "" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args).unwrap();
    assert!(result.output.contains("Applied 1 edit"));

    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "Hello");
}

#[test]
fn error_on_binary_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.bin");
    fs::write(&file_path, [0u8, 1, 2, 3, 0, 4, 5]).unwrap(); // Contains null bytes

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "test", "new_text": "replacement" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("binary file"));
}

#[test]
fn error_on_null_byte_after_initial_8k() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    let mut content = vec![b'a'; 8192];
    content.extend_from_slice(b"\0tail");
    fs::write(&file_path, content).unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "tail", "new_text": "replacement" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("detected null bytes"),
        "Error should mention null bytes: {err}"
    );
}

#[test]
fn error_on_invalid_utf8_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, b"valid text \xFF more text").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "valid", "new_text": "changed" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("File contains invalid UTF-8"),
        "Error should mention invalid UTF-8: {err}"
    );
}

#[test]
fn error_on_nonexistent_file() {
    let args = serde_json::json!({
        "path": "/etc/nonexistent_file_12345.txt",
        "edits": [
            { "old_text": "test", "new_text": "replacement" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}

#[test]
fn error_on_directory() {
    let temp_dir = TempDir::new().unwrap();

    let args = serde_json::json!({
        "path": temp_dir.path().to_str().unwrap(),
        "edits": [
            { "old_text": "test", "new_text": "replacement" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not a file"));
}

// =========================================================================
// Path Validation Tests
// =========================================================================

#[test]
fn error_on_path_outside_working_directory() {
    let args = serde_json::json!({
        "path": "/etc/passwd",
        "edits": [
            { "old_text": "test", "new_text": "replacement" }
        ]
    })
    .to_string();

    let result = execute_edit(&ToolContext::from_current_process(), &args);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .contains("outside the working directory")
    );
}

// =========================================================================
// Reverse Order Application Tests
// =========================================================================

#[test]
fn edits_applied_in_correct_positions() {
    // This test verifies that edits are applied in reverse order
    // so that position shifts don't affect later edits
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "AAA BBB CCC").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "AAA", "new_text": "XXX" },
            { "old_text": "CCC", "new_text": "ZZZ" }
        ]
    })
    .to_string();

    let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "XXX BBB ZZZ");
}

// =========================================================================
// Multi-Edit Failure Recovery Tests
// =========================================================================

#[test]
fn multi_edit_failure_reports_atomic_rollback() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    let original = "alpha\nbeta\ngamma\n";
    fs::write(&file_path, original).unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "alpha", "new_text": "ALPHA" },
            { "old_text": "beta", "new_text": "BETA" },
            { "old_text": "delta", "new_text": "DELTA" }
        ]
    })
    .to_string();

    let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();

    // Failed edit number and total are visible
    assert!(
        err.contains("Edit 3 of 3"),
        "Error should identify failing edit number and total: {err}"
    );
    // Path is visible
    assert!(
        err.contains(file_path.to_str().unwrap()),
        "Error should include the file path: {err}"
    );
    // Atomicity is explicit
    assert!(
        err.contains("No edits were applied"),
        "Error should state that no edits were applied: {err}"
    );
    assert!(
        err.contains("atomic"),
        "Error should state that the operation is atomic: {err}"
    );
    // Recovery hint
    assert!(
        err.contains("Retry"),
        "Error should suggest a retry strategy: {err}"
    );

    // File is unchanged on disk (proving rollback)
    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, original, "File should be unchanged after failure");
}

#[test]
fn multi_edit_failure_reports_per_edit_preflight_statuses() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    let original = "alpha\nbeta\ngamma\n";
    fs::write(&file_path, original).unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "alpha", "new_text": "ALPHA" },
            { "old_text": "delta", "new_text": "DELTA" },
            { "old_text": "gamma", "new_text": "GAMMA" }
        ]
    })
    .to_string();

    let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
    assert!(
        err.contains("Preflight summary:"),
        "Error should include a preflight summary: {err}"
    );
    assert!(
        err.contains("Edit 1 of 3: matched exactly once."),
        "Error should report the prior successful match: {err}"
    );
    assert!(
        err.contains("Edit 2 of 3: failed: could not find the exact text to replace."),
        "Error should report the failed edit status: {err}"
    );
    assert!(
        err.contains("Edit 3 of 3: not evaluated after the failure."),
        "Error should report that later edits were skipped: {err}"
    );
    assert!(
        err.contains("No edits were applied"),
        "Error should state that no edits were applied: {err}"
    );

    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, original, "File should be unchanged after failure");
}

#[test]
fn multi_edit_preflight_summary_is_bounded() {
    let matched_edits: Vec<_> = (1..=9)
        .map(|edit_index| MatchedEdit {
            new_text: String::new(),
            index: edit_index * 10,
            match_length: 1,
            edit_index,
        })
        .collect();

    let summary = preflight_failure_summary(10, &matched_edits, &[], 10, "failed: missing");
    assert!(
        summary.contains("Edit 1 of 10: matched exactly once."),
        "Summary should include early matched edits: {summary}"
    );
    assert!(
        summary.contains("5 matched edits omitted."),
        "Summary should cap matched status lines: {summary}"
    );
    assert!(
        summary.contains("Edit 10 of 10: failed: missing."),
        "Summary should always include the failed edit: {summary}"
    );
    assert!(
        !summary.contains("Edit 7 of 10: matched exactly once."),
        "Summary should omit excess matched status lines: {summary}"
    );
}

#[test]
fn multi_edit_ambiguous_match_reports_atomic_rollback() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    let original = "foo\nbar\nfoo\n";
    fs::write(&file_path, original).unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "bar", "new_text": "BAR" },
            { "old_text": "foo", "new_text": "FOO" }
        ]
    })
    .to_string();

    let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
    assert!(
        err.contains("Edit 2 of 2"),
        "Error should identify failing edit number and total: {err}"
    );
    assert!(
        err.contains("matches 2 locations"),
        "Error should describe the ambiguity: {err}"
    );
    assert!(
        err.contains("No edits were applied"),
        "Error should state that no edits were applied: {err}"
    );
    assert!(
        err.contains("atomic"),
        "Error should state atomic semantics for multi-edit: {err}"
    );

    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, original, "File should be unchanged after failure");
}

#[test]
fn multi_edit_overlap_reports_atomic_rollback_with_path() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    let original = "Hello world";
    fs::write(&file_path, original).unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "Hello", "new_text": "Hi" },
            { "old_text": "lo wor", "new_text": "xx" }
        ]
    })
    .to_string();

    let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
    assert!(
        err.contains("overlap"),
        "Error should mention overlap: {err}"
    );
    assert!(
        err.contains(file_path.to_str().unwrap()),
        "Error should include the file path: {err}"
    );
    assert!(
        err.contains("No edits were applied"),
        "Error should state that no edits were applied: {err}"
    );
    assert!(
        err.contains("atomic"),
        "Error should state atomic semantics for multi-edit: {err}"
    );

    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, original, "File should be unchanged after failure");
}

#[test]
fn single_edit_failure_omits_atomic_phrasing() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "missing", "new_text": "x" }
        ]
    })
    .to_string();

    let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
    assert!(
        err.contains("No edits were applied"),
        "Single-edit failure should still note no edits were applied: {err}"
    );
    assert!(
        !err.contains("atomic"),
        "Single-edit failure should not include the atomic-rollback phrasing: {err}"
    );
}

#[test]
fn multi_edit_failure_includes_nearest_match_hint() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    let original =
        "fn alpha(x: i32) -> i32 {\n    x + 1\n}\n\nfn beta(y: i32) -> i32 {\n    y * 2\n}\n";
    fs::write(&file_path, original).unwrap();

    // Search text close to a real line so the hint kicks in.
    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "fn alpha(x: u32) -> i32 {", "new_text": "fn alpha(x: i64) -> i64 {" }
        ]
    })
    .to_string();

    let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
    assert!(
        err.contains("Nearest matching context"),
        "Error should include a nearest-match hint: {err}"
    );
    assert!(
        err.contains("fn alpha"),
        "Hint should reference the closest line: {err}"
    );
}

#[test]
fn multiple_edits_with_different_lengths() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "short medium verylong").unwrap();

    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "short", "new_text": "s" },
            { "old_text": "medium", "new_text": "MEDIUM" },
            { "old_text": "verylong", "new_text": "vl" }
        ]
    })
    .to_string();

    let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "s MEDIUM vl");
}

// =========================================================================
// JSON Argument Repair Tests
//
// These tests verify that recoverable invalid JSON in tool-call arguments is
// repaired before parsing, so common LLM output errors (raw control chars in
// strings, trailing garbage) do not cause spurious failures.
// =========================================================================

#[test]
fn raw_tab_in_old_text_is_repaired() {
    // Payload with a literal tab character inside old_text — serde_json would
    // reject this, but the repair pass escapes it to \t before parsing.
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "\tHello\n").unwrap();

    let args = "{\"path\":\"".to_string()
        + file_path.to_str().unwrap()
        + "\",\"edits\":[{\"old_text\":\"\t\",\"new_text\":\"tab\"}]}";

    let result = execute_edit(&ToolContext::from_current_process(), &args).unwrap();
    assert!(result.output.contains("Applied 1 edit"));
    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "tabHello\n");
}

#[test]
fn raw_newline_in_old_text_is_repaired() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello\nworld\n").unwrap();

    // Build payload with a literal newline inside old_text
    let mut args = String::from("{\"path\":\"");
    args.push_str(file_path.to_str().unwrap());
    args.push_str("\",\"edits\":[{\"old_text\":\"Hello\n\",\"new_text\":\"Hi\"}]}");

    let result = execute_edit(&ToolContext::from_current_process(), &args).unwrap();
    assert!(result.output.contains("Applied 1 edit"));
    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "Hiworld\n");
}

#[test]
fn trailing_curly_brace_is_ignored() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world\n").unwrap();

    // Double-closed object: extra } at the end
    let args = serde_json::json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            { "old_text": "Hello", "new_text": "Hi" }
        ]
    })
    .to_string()
        + "}";

    let result = execute_edit(&ToolContext::from_current_process(), &args).unwrap();
    assert!(result.output.contains("Applied 1 edit"));
    let content = fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "Hi world\n");
}

#[test]
fn repaired_but_wrong_content_still_fails_preflight_match() {
    // Repair succeeds syntactically, but old_text doesn't match file content
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world\n").unwrap();

    let args = "{\"path\":\"".to_string()
        + file_path.to_str().unwrap()
        + "\",\"edits\":[{\"old_text\":\"	\",\"new_text\":\"Hi\"}]}";

    let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
    assert!(
        err.contains("could not find the exact text to replace"),
        "Repaired-but-non-matching old_text should fail preflight: {err}"
    );
}

#[test]
fn quote_desync_payload_still_rejected() {
    // Unrepairable quote-desync should produce the same error as before
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello\n").unwrap();

    let args = "{\"path\":\"".to_string()
        + file_path.to_str().unwrap()
        + "\",\"edits\":[{\"old_text\":\"Hello\",\"new_text\":\"Hi]}";

    let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
    assert!(
        err.contains("Invalid edit arguments"),
        "Unrepairable payload should produce Invalid edit arguments: {err}"
    );
}
