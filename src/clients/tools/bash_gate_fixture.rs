//! Deterministic gate fixture shared by Bash enforcement tests.
//!
//! The module is compiled once for all test modules, so helpers a given module
//! does not use must not fail the strict `-D warnings` build.
#![allow(
    dead_code,
    reason = "shared test fixture; each test module uses a subset"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::gates::composer::{
    FailMode, Gate, GateAnswer, GateFailure, GateFuture, GateMode, GatePipeline, GateStageConfig,
    GateStartup,
};
use crate::gates::protocol::{
    CheckFrame, DecisionFrame, Describe, GateAction, GateDecision, GatePhase, GateTransport,
    PROTOCOL_VERSION,
};

pub struct CapturedPipeline {
    pipeline: Arc<GatePipeline>,
    frames: Arc<Mutex<Vec<CheckFrame>>>,
}

impl CapturedPipeline {
    pub fn pipeline(&self) -> Arc<GatePipeline> {
        Arc::clone(&self.pipeline)
    }

    pub fn frames(&self) -> Vec<CheckFrame> {
        self.frames.lock().expect("fake gate frame lock").clone()
    }
}

#[derive(Clone)]
enum Reply {
    Decision {
        decision: GateDecision,
        code: Option<String>,
        message: String,
    },
    Failure(GateFailure),
}

struct FakeGate {
    describe: Describe,
    reply: Reply,
    frames: Arc<Mutex<Vec<CheckFrame>>>,
}

impl std::fmt::Debug for FakeGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FakeGate")
            .field("describe", &self.describe)
            .finish_non_exhaustive()
    }
}

impl Gate for FakeGate {
    fn describe(&self) -> &Describe {
        &self.describe
    }

    fn check(&self, frame: CheckFrame) -> GateFuture<'_> {
        self.frames
            .lock()
            .expect("fake gate frame lock")
            .push(frame.clone());
        let reply = self.reply.clone();
        Box::pin(async move {
            match reply {
                Reply::Decision {
                    decision,
                    code,
                    message,
                } => {
                    let mut frame = DecisionFrame::new(frame.id, decision).with_message(message);
                    if let Some(code) = code {
                        frame = frame.with_code(code);
                    }
                    GateAnswer::decision(frame, Duration::from_millis(2))
                },
                Reply::Failure(failure) => {
                    GateAnswer::failure(failure, false, Duration::from_millis(2))
                },
            }
        })
    }
}

pub fn decision(decision: GateDecision, code: Option<&str>, message: &str) -> CapturedPipeline {
    configured(
        GateStageConfig {
            name: "fake-safety".to_string(),
            mode: GateMode::Enforce,
            fail: FailMode::Closed,
            short_circuit_on_allow: false,
            allowlist: Vec::new(),
        },
        Reply::Decision {
            decision,
            code: code.map(str::to_string),
            message: message.to_string(),
        },
    )
}

pub fn failing(class: &str, message: &str) -> CapturedPipeline {
    configured(
        GateStageConfig::enforce("fake-safety"),
        Reply::Failure(GateFailure::new(
            crate::gates::protocol::GateErrorClass::from_wire(class),
            message,
        )),
    )
}

pub fn with_allowlist(command: &str) -> CapturedPipeline {
    let mut config = GateStageConfig::enforce("fake-safety");
    config.allowlist.push(command.to_string());
    configured(
        config,
        Reply::Decision {
            decision: GateDecision::Block,
            code: Some("fake-block".to_string()),
            message: "blocked by fixture".to_string(),
        },
    )
}

fn configured(config: GateStageConfig, reply: Reply) -> CapturedPipeline {
    let frames = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(FakeGate {
        describe: Describe {
            protocol: PROTOCOL_VERSION.to_string(),
            name: "fake-safety".to_string(),
            version: "1.0.0-test".to_string(),
            phases: vec![GatePhase::PreTool],
            actions: vec![GateAction::BashExec],
            transports: vec![GateTransport::Stdio],
            capabilities: Vec::new(),
        },
        reply,
        frames: Arc::clone(&frames),
    });
    let pipeline = GatePipeline::single(GateStartup::ready(config, gate, Duration::from_millis(1)))
        .expect("valid fake gate pipeline");
    CapturedPipeline {
        pipeline: Arc::new(pipeline),
        frames,
    }
}
