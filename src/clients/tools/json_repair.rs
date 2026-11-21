//! Conservative repair of recoverable invalid JSON from LLM tool-call arguments.
//!
//! Repairs run only after strict `serde_json` parsing fails. Each repair is
//! deterministic and lossless: escaping raw control characters preserves the
//! decoded string content. Truncating trailing data, quote-desynchronization,
//! and any repair that requires guessing intent are deliberately out of scope.

/// Attempt to repair common recoverable JSON issues from LLM tool-call
/// arguments.
///
/// Returns the repaired text if any repair was applied and the result is valid
/// JSON. Returns the original payload unchanged if:
/// - It was already valid JSON.
/// - No repair produced valid JSON (unrepairable — caller gets the original
///   error path).
///
/// Raw control characters inside string literals (U+0000–U+001F) are replaced
/// with their JSON escape sequences (`\t`, `\n`, `\r`, `\u00XX`). Characters
/// outside strings are never touched. Trailing data is never discarded: the
/// complete repaired payload must parse, otherwise callers receive the original
/// payload and their normal parse error.
pub fn repair_json_args(payload: &str) -> String {
    if serde_json::from_str::<serde_json::Value>(payload).is_ok() {
        return payload.to_string();
    }

    let control_fixed = escape_control_chars_in_strings(payload);
    if serde_json::from_str::<serde_json::Value>(&control_fixed).is_ok() {
        return control_fixed;
    }

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
            ch if in_string && (ch as u32) <= 0x1F => match ch {
                '\t' => result.push_str("\\t"),
                '\n' => result.push_str("\\n"),
                '\r' => result.push_str("\\r"),
                other => {
                    use std::fmt::Write as _;
                    #[expect(
                        clippy::let_underscore_must_use,
                        reason = "write! to String cannot fail"
                    )]
                    let _ = write!(result, "\\u{:04x}", other as u32);
                },
            },
            _ => {
                result.push(ch);
            },
        }
    }

    result
}
