use std::sync::Arc;

use crate::clients::agent::Agent;
use crate::session_telemetry::{
    AgentRunnerTelemetryEvent, CompensationEventTelemetry, SessionTelemetryContext,
    SessionTelemetryRecord, SharedSessionTelemetryWriter, TelemetryAppend, ToolCallTelemetry,
};

pub(super) struct AgentRunnerTelemetrySink {
    context: SessionTelemetryContext,
    writer: Arc<SharedSessionTelemetryWriter>,
}

impl AgentRunnerTelemetrySink {
    pub(super) fn record(&self, event: AgentRunnerTelemetryEvent) {
        let record = runner_telemetry_record(self.context.clone(), event);
        if let TelemetryAppend::Failed(error) = self.writer.append(&record) {
            tracing::warn!(
                target: "cake",
                "Disabling session telemetry after write failure: {error}"
            );
        }
    }
}

impl Agent {
    pub(super) fn telemetry_context(&self) -> Option<SessionTelemetryContext> {
        self.telemetry
            .as_ref()
            .map(|telemetry| telemetry.context.clone())
    }

    pub(super) fn runner_telemetry_sink(&self) -> Option<AgentRunnerTelemetrySink> {
        self.telemetry
            .as_ref()
            .map(|telemetry| AgentRunnerTelemetrySink {
                context: telemetry.context.clone(),
                writer: Arc::clone(&telemetry.writer),
            })
    }

    pub(super) fn append_runner_telemetry(&mut self, event: AgentRunnerTelemetryEvent) {
        let Some(context) = self.telemetry_context() else {
            return;
        };
        let record = runner_telemetry_record(context, event);
        self.append_telemetry_record(&record);
    }

    pub(super) fn append_tool_call_telemetry(&mut self, tool_call: ToolCallTelemetry) {
        let Some(context) = self.telemetry_context() else {
            return;
        };
        let record = SessionTelemetryRecord::ToolCall {
            session_id: context.session_id,
            invocation_id: context.invocation_id,
            timestamp: chrono::Utc::now(),
            tool_call,
        };
        self.append_telemetry_record(&record);
    }

    pub(super) fn append_compensation_telemetry(&mut self, event: CompensationEventTelemetry) {
        let Some(context) = self.telemetry_context() else {
            return;
        };
        let record = SessionTelemetryRecord::Compensation {
            session_id: context.session_id,
            invocation_id: context.invocation_id,
            timestamp: chrono::Utc::now(),
            event,
        };
        self.append_telemetry_record(&record);
    }

    pub(super) fn append_telemetry_record(&mut self, record: &SessionTelemetryRecord) {
        let Some(telemetry) = &self.telemetry else {
            return;
        };
        if let TelemetryAppend::Failed(error) = telemetry.writer.append(record) {
            tracing::warn!(
                target: "cake",
                "Disabling session telemetry after write failure: {error}"
            );
            self.telemetry = None;
        }
    }
}

fn runner_telemetry_record(
    context: SessionTelemetryContext,
    event: AgentRunnerTelemetryEvent,
) -> SessionTelemetryRecord {
    match event {
        AgentRunnerTelemetryEvent::ApiAttempt(attempt) => SessionTelemetryRecord::ApiAttempt {
            session_id: context.session_id,
            invocation_id: context.invocation_id,
            timestamp: chrono::Utc::now(),
            attempt,
        },
        AgentRunnerTelemetryEvent::RetryScheduled(retry) => {
            SessionTelemetryRecord::RetryScheduled {
                session_id: context.session_id,
                invocation_id: context.invocation_id,
                timestamp: chrono::Utc::now(),
                retry,
            }
        },
        AgentRunnerTelemetryEvent::Compensation(event) => SessionTelemetryRecord::Compensation {
            session_id: context.session_id,
            invocation_id: context.invocation_id,
            timestamp: chrono::Utc::now(),
            event,
        },
    }
}
