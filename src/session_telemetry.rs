use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::OutputFormat;
use crate::clients::retry::{RequestOverrides, RetryReason, RetryStatus};
use crate::clients::types::Usage;
use crate::config::model::{ApiType, ReasoningEffort};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTelemetryRunMode {
    New,
    Continue,
    Resume,
    Fork,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionTelemetrySettings {
    pub api_type: ApiType,
    pub output_format: OutputFormat,
    pub max_output_tokens: Option<u32>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionTelemetryContext {
    pub session_id: String,
    pub invocation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiAttemptTelemetry {
    pub turn_index: u32,
    pub attempt: u32,
    pub request_ms: u64,
    pub parse_ms: u64,
    pub total_ms: u64,
    pub history_items: usize,
    pub status_code: Option<u16>,
    pub error: Option<String>,
    pub usage: Option<Usage>,
    pub request_overrides: RequestOverridesSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetryScheduledTelemetry {
    pub turn_index: u32,
    pub attempt: u32,
    pub max_retries: u32,
    pub reason: RetryReasonSnapshot,
    pub delay_ms: u64,
    pub detail: String,
    pub changed_request_overrides: bool,
    pub request_overrides: RequestOverridesSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallTelemetry {
    pub turn_index: u32,
    pub call_id: String,
    pub name: String,
    pub duration_ms: u64,
    pub output_bytes: usize,
    pub was_error: bool,
}

#[derive(Debug, Clone)]
pub enum AgentRunnerTelemetryEvent {
    ApiAttempt(ApiAttemptTelemetry),
    RetryScheduled(RetryScheduledTelemetry),
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestOverridesSnapshot {
    pub max_output_tokens: Option<u32>,
    pub reasoning_max_tokens: Option<u32>,
    pub context_overflow_retry_used: bool,
}

impl From<&RequestOverrides> for RequestOverridesSnapshot {
    fn from(overrides: &RequestOverrides) -> Self {
        Self {
            max_output_tokens: overrides.max_output_tokens,
            reasoning_max_tokens: overrides.reasoning_max_tokens,
            context_overflow_retry_used: overrides.context_overflow_retry_used,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryReasonSnapshot {
    RateLimit,
    Overloaded,
    ServerError,
    RequestTimeout,
    LockTimeout,
    Network,
    ContextOverflow,
}

impl From<&RetryReason> for RetryReasonSnapshot {
    fn from(reason: &RetryReason) -> Self {
        match reason {
            RetryReason::RateLimit => Self::RateLimit,
            RetryReason::Overloaded => Self::Overloaded,
            RetryReason::ServerError => Self::ServerError,
            RetryReason::RequestTimeout => Self::RequestTimeout,
            RetryReason::LockTimeout => Self::LockTimeout,
            RetryReason::Network => Self::Network,
            RetryReason::ContextOverflow => Self::ContextOverflow,
        }
    }
}

impl RetryScheduledTelemetry {
    pub fn from_status(
        status: &RetryStatus,
        turn_index: u32,
        changed_request_overrides: bool,
        request_overrides: &RequestOverrides,
    ) -> Self {
        Self {
            turn_index,
            attempt: status.attempt,
            max_retries: status.max_retries,
            reason: RetryReasonSnapshot::from(&status.reason),
            delay_ms: status.delay.as_millis().try_into().unwrap_or(u64::MAX),
            detail: status.detail.clone(),
            changed_request_overrides,
            request_overrides: RequestOverridesSnapshot::from(request_overrides),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionTelemetryRecord {
    TelemetryInit {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        mode: SessionTelemetryRunMode,
        working_directory: String,
        model: String,
        api_type: ApiType,
        output_format: OutputFormat,
        tools: Vec<String>,
        settings: SessionTelemetrySettings,
    },
    ApiAttempt {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        attempt: ApiAttemptTelemetry,
    },
    RetryScheduled {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        retry: RetryScheduledTelemetry,
    },
    ToolCall {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        tool_call: ToolCallTelemetry,
    },
    SessionSummary {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        success: bool,
        duration_ms: u64,
        turn_count: u32,
        usage: Usage,
        error: Option<String>,
    },
}

pub struct SessionTelemetryWriter {
    writer: BufWriter<File>,
}

impl SessionTelemetryWriter {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    pub fn append(&mut self, record: &SessionTelemetryRecord) -> anyhow::Result<()> {
        serde_json::to_writer(&mut self.writer, record)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_appends_newline_delimited_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session-telemetry").join("test.ndjson");
        let session_id = uuid::Uuid::new_v4().to_string();
        let invocation_id = uuid::Uuid::new_v4().to_string();
        let mut writer = SessionTelemetryWriter::open(&path).unwrap();

        writer
            .append(&SessionTelemetryRecord::SessionSummary {
                session_id: session_id.clone(),
                invocation_id: invocation_id.clone(),
                timestamp: Utc::now(),
                success: true,
                duration_ms: 42,
                turn_count: 1,
                usage: Usage::default(),
                error: None,
            })
            .unwrap();
        writer
            .append(&SessionTelemetryRecord::SessionSummary {
                session_id,
                invocation_id,
                timestamp: Utc::now(),
                success: false,
                duration_ms: 99,
                turn_count: 2,
                usage: Usage::default(),
                error: Some("boom".to_string()),
            })
            .unwrap();
        drop(writer);

        let contents = std::fs::read_to_string(path).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[0]).unwrap()["type"],
            "session_summary"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[1]).unwrap()["error"],
            "boom"
        );
        assert!(contents.ends_with('\n'));
    }
}
