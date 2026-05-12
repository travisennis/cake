//! Shared time formatting helpers for CLI-facing output.

use std::time::Duration;

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

/// Format a duration as seconds with one decimal place using integer rounding.
pub fn format_duration_tenths(duration: Duration) -> String {
    format_seconds_tenths(duration.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_tenths_rounds_to_nearest_tenth() {
        assert_eq!(format_seconds_tenths(0), "0.0");
        assert_eq!(format_seconds_tenths(49), "0.0");
        assert_eq!(format_seconds_tenths(50), "0.1");
        assert_eq!(format_seconds_tenths(1_000), "1.0");
        assert_eq!(format_seconds_tenths(1_049), "1.0");
        assert_eq!(format_seconds_tenths(1_050), "1.1");
        assert_eq!(format_seconds_tenths(1_234), "1.2");
        assert_eq!(format_seconds_tenths(1_499), "1.5");
        assert_eq!(format_seconds_tenths(1_500), "1.5");
    }

    #[test]
    fn duration_tenths_formats_milliseconds_as_seconds() {
        assert_eq!(format_duration_tenths(Duration::from_millis(1_250)), "1.3");
    }

    #[test]
    fn seconds_tenths_handles_max_milliseconds_without_overflowing() {
        let formatted = format_seconds_tenths(u128::MAX);

        assert!(formatted.contains('.'));
    }
}
