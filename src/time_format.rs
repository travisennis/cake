//! Shared time formatting helpers for CLI-facing output.

const MILLIS_PER_TENTH_SECOND: u128 = 100;
const TENTHS_PER_SECOND: u128 = 10;

/// Format milliseconds as seconds with one decimal place using integer rounding.
pub fn format_seconds_tenths(elapsed_ms: u128) -> String {
    let rounded_tenths =
        elapsed_ms.saturating_add(MILLIS_PER_TENTH_SECOND / 2) / MILLIS_PER_TENTH_SECOND;
    format!(
        "{}.{:01}",
        rounded_tenths / TENTHS_PER_SECOND,
        rounded_tenths % TENTHS_PER_SECOND
    )
}
