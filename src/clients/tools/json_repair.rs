//! Conservative repair of recoverable invalid JSON from LLM tool-call arguments.
//!
//! Repairs run only after strict `serde_json` parsing fails. Each repair is
//! deterministic and lossless — if a repair produces the "wrong" string content,
//! the tool's own preflight validation (e.g., exact-match checking in Edit)
//! still catches it. Quote-desynchronization or any repair that requires
//! guessing intent is deliberately out of scope.

/// Attempt to repair common recoverable JSON issues from LLM tool-call
/// arguments.
///
/// Returns the repaired text if any repair was applied and the result is valid
/// JSON. Returns the original payload unchanged if:
/// - It was already valid JSON.
/// - No repair produced valid JSON (unrepairable — caller gets the original
///   error path).
///
/// Current repairs (applied sequentially):
///
/// 1. **Raw control characters inside string literals** — Literal tab, newline,
///    carriage return, and other U+0000–U+001F characters found *inside* JSON
///    string values are replaced with their JSON escape sequences (`\t`, `\n`,
///    `\r`, `\u00XX`). Characters outside strings are never touched.
///
/// 2. **Trailing garbage after a balanced top-level object** — If the payload
///    contains a complete JSON value followed by extra data (e.g., a doubled
///    `}` or provider markup), the first complete value is kept and the
///    remainder discarded.
pub fn repair_json_args(payload: &str) -> String {
    // Fast path: if already valid, return as-is
    if serde_json::from_str::<serde_json::Value>(payload).is_ok() {
        return payload.to_string();
    }

    // Step 1: Escape raw control characters inside string literals
    let control_fixed = escape_control_chars_in_strings(payload);
    if serde_json::from_str::<serde_json::Value>(&control_fixed).is_ok() {
        return control_fixed;
    }

    // Step 2: Try trailing-data recovery (first complete value)
    if let Some(truncated) = try_first_complete_value(&control_fixed)
        && serde_json::from_str::<serde_json::Value>(&truncated).is_ok()
    {
        return truncated;
    }

    // Unrepairable — return original so caller gets the same error
    payload.to_string()
}

/// Replace raw control characters (U+0000–U+001F) found inside JSON string
/// literals with their JSON escape sequences. Characters outside strings are
/// left untouched.
///
/// Uses a small state machine tracking in-string/escape state to correctly
/// handle escaped quotes and backslashes.
fn escape_control_chars_in_strings(payload: &str) -> String {
    let mut result = String::with_capacity(payload.len());
    let mut in_string = false;
    let mut escaped = false;

    for ch in payload.chars() {
        match ch {
            '\\' if in_string && !escaped => {
                escaped = true;
                result.push(ch);
            },
            '"' if !escaped => {
                in_string = !in_string;
                result.push(ch);
            },
            _ if escaped => {
                escaped = false;
                result.push(ch);
            },
            ch if in_string && (ch as u32) <= 0x1F => {
                // Raw control character inside a string — replace with escape
                match ch {
                    '\t' => result.push_str("\\t"),
                    '\n' => result.push_str("\\n"),
                    '\r' => result.push_str("\\r"),
                    other => {
                        use std::fmt::Write as _;
                        // Format as \u00XX for other control chars
                        #[expect(
                            clippy::let_underscore_must_use,
                            reason = "write! to String cannot fail"
                        )]
                        let _ = write!(result, "\\u{:04x}", other as u32);
                    },
                }
            },
            _ => {
                result.push(ch);
            },
        }
    }

    result
}

/// Try to extract the first complete top-level JSON value from a payload that
/// might have trailing garbage.
///
/// Returns `None` if no complete value can be parsed, or if the payload ends
/// exactly at the end of the first value (no trailing data).
fn try_first_complete_value(payload: &str) -> Option<String> {
    use serde_json::Value;
    let mut stream = serde_json::Deserializer::from_str(payload).into_iter::<Value>();
    match stream.next()? {
        Ok(_) => {
            let end = stream.byte_offset();
            if end < payload.len() {
                payload.get(..end).map(str::to_string)
            } else {
                None
            }
        },
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Valid payloads pass through unchanged ──

    #[test]
    fn valid_json_passes_through_unchanged() {
        let payload = r#"{"path":"f","edits":[{"old_text":"hello","new_text":"hi"}]}"#;
        let repaired = repair_json_args(payload);
        assert_eq!(repaired, payload);
    }

    #[test]
    fn already_escaped_control_chars_untouched() {
        let payload = r#"{"path":"f","edits":[{"old_text":"hello\tworld","new_text":"hi"}]}"#;
        let repaired = repair_json_args(payload);
        assert_eq!(repaired, payload);
    }

    #[test]
    fn escaped_quotes_do_not_confuse_state_machine() {
        let payload = r#"{"path":"f","edits":[{"old_text":"he said \"hello\"","new_text":"hi"}]}"#;
        let repaired = repair_json_args(payload);
        assert_eq!(repaired, payload);
    }

    #[test]
    fn escaped_backslashes_do_not_confuse_state_machine() {
        let payload = r#"{"path":"f","edits":[{"old_text":"path\\to\\file","new_text":"new"}]}"#;
        let repaired = repair_json_args(payload);
        assert_eq!(repaired, payload);
    }

    // ── Control character repair ──

    #[test]
    fn raw_tab_inside_string_is_escaped() {
        let payload = "{\"path\":\"f\",\"edits\":[{\"old_text\":\"\t\",\"new_text\":\"x\"}]}";
        let repaired = repair_json_args(payload);
        assert!(
            serde_json::from_str::<serde_json::Value>(&repaired).is_ok(),
            "repaired payload should be valid JSON"
        );
        assert!(!repaired.contains('\t'), "raw tab should be replaced");
        assert!(repaired.contains("\\t"), "escaped tab should be present");
    }

    #[test]
    fn raw_newline_inside_string_is_escaped() {
        let payload = "{\"path\":\"f\",\"edits\":[{\"old_text\":\"\n\",\"new_text\":\"x\"}]}";
        let repaired = repair_json_args(payload);
        assert!(serde_json::from_str::<serde_json::Value>(&repaired).is_ok());
        assert!(!repaired.contains('\n'), "raw newline should be replaced");
        assert!(
            repaired.contains("\\n"),
            "escaped newline should be present"
        );
    }

    #[test]
    fn raw_carriage_return_inside_string_is_escaped() {
        let payload = "{\"path\":\"f\",\"edits\":[{\"old_text\":\"\r\",\"new_text\":\"x\"}]}";
        let repaired = repair_json_args(payload);
        assert!(serde_json::from_str::<serde_json::Value>(&repaired).is_ok());
        assert!(!repaired.contains('\r'), "raw CR should be replaced");
        assert!(repaired.contains("\\r"), "escaped CR should be present");
    }

    #[test]
    fn control_chars_outside_strings_untouched() {
        // Tab and newline in JSON whitespace are valid and must not change
        let payload = "{\n\t\"path\": \"f\",\n\t\"edits\": []\n}";
        let repaired = repair_json_args(payload);
        assert_eq!(repaired, payload, "valid JSON with whitespace unchanged");
    }

    #[test]
    fn multiple_control_chars_in_one_payload() {
        let payload = "{\"path\":\"f\",\"edits\":[{\"old_text\":\"\t\n\r\",\"new_text\":\"x\"}]}";
        let repaired = repair_json_args(payload);
        assert!(serde_json::from_str::<serde_json::Value>(&repaired).is_ok());
        assert!(!repaired.contains('\t'));
        assert!(!repaired.contains('\n'));
        assert!(!repaired.contains('\r'));
        assert!(repaired.contains("\\t"));
        assert!(repaired.contains("\\n"));
        assert!(repaired.contains("\\r"));
    }

    #[test]
    fn null_byte_inside_string_is_escaped() {
        let payload = "{\"path\":\"f\",\"edits\":[{\"old_text\":\"\0\",\"new_text\":\"x\"}]}";
        let repaired = repair_json_args(payload);
        assert!(serde_json::from_str::<serde_json::Value>(&repaired).is_ok());
        assert!(repaired.contains("\\u0000"));
    }

    // ── Trailing-data repair ──

    #[test]
    fn trailing_curly_brace_is_removed() {
        let payload = r#"{"path":"f","edits":[{"old_text":"a","new_text":"b"}]}}"#;
        let repaired = repair_json_args(payload);
        assert!(serde_json::from_str::<serde_json::Value>(&repaired).is_ok());
        let parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed["path"], "f");
    }

    #[test]
    fn trailing_markup_is_removed() {
        let payload = r#"{"path":"f","edits":[{"old_text":"a","new_text":"b"}]} some text"#;
        let repaired = repair_json_args(payload);
        assert!(serde_json::from_str::<serde_json::Value>(&repaired).is_ok());
    }

    // ── Combined repairs ──

    #[test]
    fn control_chars_and_trailing_data_both_fixed() {
        let payload = "{\"path\":\"f\",\"edits\":[{\"old_text\":\"\t\",\"new_text\":\"b\"}]}xxx";
        let repaired = repair_json_args(payload);
        assert!(serde_json::from_str::<serde_json::Value>(&repaired).is_ok());
    }

    // ── Unrepairable payloads ──

    #[test]
    fn quote_desync_still_fails() {
        // Unclosed outer object — missing final `}`
        let payload = r#"{"path":"f","edits":[{"old_text":"a","new_text":"b"}]"#;
        let repaired = repair_json_args(payload);
        assert_eq!(repaired, payload, "unrepairable payload returned unchanged");
        assert!(serde_json::from_str::<serde_json::Value>(&repaired).is_err());
    }

    #[test]
    fn completely_garbled_still_fails() {
        let payload = r"this is not json at all";
        let repaired = repair_json_args(payload);
        assert_eq!(repaired, payload);
        assert!(serde_json::from_str::<serde_json::Value>(&repaired).is_err());
    }

    // ── Trailing data applied to already-repaired control chars ──

    #[test]
    fn trailing_after_control_char_repair() {
        // Control chars need fixing, AND there's trailing data
        let payload = "{\"path\":\"f\",\"edits\":[{\"old_text\":\"\t\",\"new_text\":\"b\"}]}xxx";
        let repaired = repair_json_args(payload);
        assert!(serde_json::from_str::<serde_json::Value>(&repaired).is_ok());
        let parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed["path"], "f");
        assert_eq!(
            parsed["edits"][0]["old_text"], "\t",
            "old_text should contain tab character after deserialization"
        );
    }
}
