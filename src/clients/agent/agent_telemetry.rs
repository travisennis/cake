use crate::clients::agent::Agent;
use crate::session_telemetry::{
    AgentRunnerTelemetryEvent, SessionTelemetryContext, SessionTelemetryRecord, ToolCallTelemetry,
};

impl Agent {
    pub(super) fn telemetry_context(&self) -> Option<SessionTelemetryContext> {
        self.telemetry
            .as_ref()
            .map(|telemetry| telemetry.context.clone())
    }

    pub(super) fn append_runner_telemetry(&mut self, event: AgentRunnerTelemetryEvent) {
        let Some(context) = self.telemetry_context() else {
            return;
        };
        let record = match event {
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
        };
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

    pub(super) fn append_telemetry_record(&mut self, record: &SessionTelemetryRecord) {
        let Some(telemetry) = &mut self.telemetry else {
            return;
        };
        if let Err(error) = telemetry.writer.append(record) {
            tracing::warn!(
                target: "cake",
                "Disabling session telemetry after write failure: {error}"
            );
            self.telemetry = None;
        }
    }
}
