//! No-data stall deadline for provider responses.
//!
//! Providers can spend minutes on a single turn --- long reasoning, slow
//! tool-argument generation, a slow proxy --- but a total wall-clock deadline
//! fails work that is still making progress. The stall deadline instead bounds
//! silence: every received chunk resets it, so a response may take as long as
//! it keeps sending data.
//!
//! The deadline covers both the wait for response headers (the request phase)
//! and the wait for the next body chunk. The connect timeout remains separate.

use std::fmt;
use std::time::Duration;

/// Longest the provider may stay silent before the attempt fails.
pub(super) const STALL_WINDOW: Duration = Duration::from_mins(10);

const PREVIEW_LIMIT: usize = 400;

/// Where the provider stopped sending data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallPhase {
    /// No response headers arrived before the deadline.
    Connect,
    /// Headers arrived, but the first body byte did not.
    FirstByte,
    /// Part of the body arrived before the deadline.
    MidBody,
}

impl StallPhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::FirstByte => "first byte",
            Self::MidBody => "mid-body",
        }
    }
}

/// A provider attempt that stayed silent past the stall deadline.
#[derive(Debug)]
pub struct ResponseStall {
    model: String,
    phase: StallPhase,
    window: Duration,
    bytes_received: usize,
    preview: Option<String>,
}

impl ResponseStall {
    pub(crate) fn new(
        model: impl Into<String>,
        phase: StallPhase,
        window: Duration,
        bytes_received: usize,
    ) -> Self {
        Self {
            model: model.into(),
            phase,
            window,
            bytes_received,
            preview: None,
        }
    }

    /// Attach a bounded preview of the bytes received before the stall.
    pub(crate) fn with_preview(mut self, body: &[u8]) -> Self {
        if !body.is_empty() {
            let preview_len = body.len().min(PREVIEW_LIMIT);
            self.preview = Some(String::from_utf8_lossy(&body[..preview_len]).into_owned());
        }
        self
    }

    /// Short detail for retry telemetry.
    pub(crate) fn retry_detail(&self) -> String {
        format!("provider stall ({})", self.phase.as_str())
    }

    /// Full diagnostic without the body preview, safe for telemetry sinks
    /// that must not carry provider response bytes.
    pub(crate) fn telemetry_detail(&self) -> String {
        self.describe(false)
    }

    fn describe(&self, include_preview: bool) -> String {
        let head = format!(
            "provider response stalled: model '{}' received no data for {} during {} \
             ({} bytes received)",
            self.model,
            format_window(self.window),
            self.phase.as_str(),
            self.bytes_received
        );
        match self.preview.as_ref().filter(|_| include_preview) {
            Some(preview) => format!("{head}; body preview: {preview:?}"),
            None => head,
        }
    }
}

impl fmt::Display for ResponseStall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe(true))
    }
}

impl std::error::Error for ResponseStall {}

fn format_window(window: Duration) -> String {
    let millis = window.as_millis();
    if millis < 1_000 {
        return format!("{millis} ms");
    }

    let seconds = window.as_secs();
    if seconds.is_multiple_of(60) {
        let minutes = seconds / 60;
        if minutes == 1 {
            "1 minute".to_string()
        } else {
            format!("{minutes} minutes")
        }
    } else if seconds == 1 {
        "1 second".to_string()
    } else {
        format!("{seconds} seconds")
    }
}

/// A body read that ended because the provider stalled.
pub(super) struct StalledBody {
    pub(super) bytes: Vec<u8>,
    pub(super) stall: ResponseStall,
}

/// A body read that ended before the complete body arrived.
pub(super) struct FailedBody {
    pub(super) bytes: Vec<u8>,
    pub(super) error: anyhow::Error,
}

/// The terminal state of reading a response body.
///
/// The bytes received before a stall or transport failure are retained so a
/// non-2xx handler can keep the provider's partial diagnostic text.
pub(super) enum BodyReadOutcome {
    Complete(Vec<u8>),
    Stalled(StalledBody),
    Failed(FailedBody),
}

/// Why a response body did not complete.
///
/// This stays typed on [`crate::exit_code::ApiError`] so status-only handling
/// can distinguish a provider verdict from a body that never arrived.
#[derive(Debug)]
pub enum BodyReadFailure {
    Stalled(ResponseStall),
    Failed(anyhow::Error),
}

impl BodyReadFailure {
    pub(crate) const fn stalled(stall: ResponseStall) -> Self {
        Self::Stalled(stall)
    }

    pub(crate) const fn failed(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }

    pub(crate) const fn is_stalled(&self) -> bool {
        matches!(self, Self::Stalled(_))
    }

    pub(crate) const fn error(&self) -> Option<&anyhow::Error> {
        match self {
            Self::Stalled(_) => None,
            Self::Failed(error) => Some(error),
        }
    }

    /// Detail without provider body bytes, safe for telemetry.
    pub(crate) fn telemetry_detail(&self) -> String {
        match self {
            Self::Stalled(stall) => stall.telemetry_detail(),
            Self::Failed(error) => format!("{error:#}"),
        }
    }

    /// Human-facing detail, including a stalled body's bounded preview.
    pub(crate) fn terminal_detail(&self) -> String {
        match self {
            Self::Stalled(stall) => stall.to_string(),
            Self::Failed(error) => format!("{error:#}"),
        }
    }
}

/// Read a response body, resetting the stall deadline on every received chunk.
///
/// Transport failures and stalls are returned as data with the bytes received
/// so far, letting callers preserve a non-2xx body's partial diagnostic text.
pub(super) async fn read_body(
    response: &mut reqwest::Response,
    model: &str,
    window: Duration,
) -> BodyReadOutcome {
    let mut bytes = Vec::new();
    loop {
        match tokio::time::timeout(window, response.chunk()).await {
            Ok(Ok(Some(chunk))) => bytes.extend_from_slice(&chunk),
            Ok(Ok(None)) => return BodyReadOutcome::Complete(bytes),
            Ok(Err(error)) => {
                return BodyReadOutcome::Failed(FailedBody {
                    bytes,
                    error: error.into(),
                });
            },
            Err(_elapsed) => {
                let phase = if bytes.is_empty() {
                    StallPhase::FirstByte
                } else {
                    StallPhase::MidBody
                };
                let bytes_received = bytes.len();
                return BodyReadOutcome::Stalled(StalledBody {
                    bytes,
                    stall: ResponseStall::new(model, phase, window, bytes_received),
                });
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_the_model_phase_window_and_bytes() {
        let stall =
            ResponseStall::new("glm-5", StallPhase::MidBody, Duration::from_mins(10), 1_234);

        assert_eq!(
            stall.to_string(),
            "provider response stalled: model 'glm-5' received no data for 10 minutes \
             during mid-body (1234 bytes received)"
        );
    }

    #[test]
    fn display_covers_subsecond_and_singular_windows() {
        assert!(
            ResponseStall::new("m", StallPhase::Connect, Duration::from_millis(250), 0)
                .to_string()
                .contains("for 250 ms during connect (0 bytes received)")
        );
        assert!(
            ResponseStall::new("m", StallPhase::FirstByte, Duration::from_secs(1), 0)
                .to_string()
                .contains("for 1 second during first byte")
        );
    }

    #[test]
    fn preview_is_bounded_and_optional() {
        let body = vec![b'a'; PREVIEW_LIMIT + 50];
        let stall = ResponseStall::new(
            "m",
            StallPhase::MidBody,
            Duration::from_secs(10),
            body.len(),
        )
        .with_preview(&body);

        let text = stall.to_string();
        assert!(text.contains("450 bytes received"));
        assert!(text.contains(&"a".repeat(PREVIEW_LIMIT)));
        assert!(!text.contains(&"a".repeat(PREVIEW_LIMIT + 1)));
        assert!(
            !ResponseStall::new("m", StallPhase::MidBody, Duration::from_secs(10), 0)
                .with_preview(&[])
                .to_string()
                .contains("body preview")
        );
    }
}
