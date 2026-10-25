//! `cake-gate/1` process transport.
//!
//! One long-lived `serve` child speaks newline-delimited JSON. The runner owns
//! the `describe` subprocess, the `init`/`ready` handshake, per-check
//! deadlines, the 1 MiB frame bound, process-group teardown, restart after a
//! per-check failure, and the two-consecutive-failure circuit breaker.
//!
//! The built-in judge does not use this module; slice 1 exercises it through
//! the fake gate fixture.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::time::timeout;

use crate::gates::composer::GateFailure;
use crate::gates::protocol::{
    CheckFrame, DecisionFrame, Describe, ErrorFrame, FrameType, GateErrorClass, InitFrame,
    MAX_ENV_NAMES, MAX_FRAME_BYTES, MAX_INIT_CONFIG_BYTES, MAX_READY_WARNINGS, PROTOCOL_VERSION,
    ReadyFrame, ShutdownFrame, bound_chars, bound_error_message, json_byte_len,
    sanitize_control_text,
};

/// Environment variables every gate receives, before its declared allowlist.
const BASE_ENV: [&str; 9] = [
    "PATH",
    "HOME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// Deadline for a best-effort shutdown wait.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

/// Deadline for the forceful phase after a cooperative signal.
const KILL_WAIT: Duration = Duration::from_secs(1);

#[cfg(unix)]
const SIGTERM: libc::c_int = libc::SIGTERM;
#[cfg(unix)]
const SIGKILL: libc::c_int = libc::SIGKILL;
#[cfg(not(unix))]
const SIGTERM: i32 = 0;
#[cfg(not(unix))]
const SIGKILL: i32 = 0;

/// How to launch one gate executable.
#[derive(Debug, Clone)]
pub struct GateProcessSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Extra environment values, already resolved from the declared allowlist.
    pub environment: BTreeMap<String, String>,
    pub startup_timeout: Duration,
    pub check_timeout: Duration,
}

impl GateProcessSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            environment: BTreeMap::new(),
            startup_timeout: Duration::from_secs(10),
            check_timeout: Duration::from_secs(30),
        }
    }

    #[must_use]
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    #[must_use]
    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        self.cwd = Some(cwd);
        self
    }

    #[must_use]
    pub fn with_environment(mut self, environment: BTreeMap<String, String>) -> Self {
        self.environment = environment;
        self
    }

    #[must_use]
    pub const fn with_timeouts(mut self, startup: Duration, check: Duration) -> Self {
        self.startup_timeout = startup;
        self.check_timeout = check;
        self
    }
}

/// A failed check or a gate the breaker has disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateCheckFailure {
    /// This check failed; the composer applies the gate's `fail` mode.
    Failure(GateFailure),
    /// The circuit breaker disabled the gate for the rest of the run. Later
    /// checks follow the gate's `fail` mode without spawning another process.
    Disabled { failure: GateFailure },
}

impl GateCheckFailure {
    pub const fn failure(&self) -> &GateFailure {
        match self {
            Self::Failure(failure) | Self::Disabled { failure } => failure,
        }
    }

    pub const fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled { .. })
    }
}

struct GateChild {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    pgid: Option<u32>,
}

/// One gate process, its cached `describe`, and its breaker state.
pub struct GateProcess {
    spec: GateProcessSpec,
    describe: Option<Describe>,
    init: Option<InitFrame>,
    child: Option<GateChild>,
    consecutive_process_failures: u8,
    disabled_failure: Option<GateFailure>,
}

impl std::fmt::Debug for GateProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GateProcess")
            .field("spec", &self.spec)
            .field("describe", &self.describe)
            .field("child", &self.child.is_some())
            .field(
                "consecutive_process_failures",
                &self.consecutive_process_failures,
            )
            .field("disabled_failure", &self.disabled_failure)
            .finish_non_exhaustive()
    }
}

impl GateProcess {
    pub const fn new(spec: GateProcessSpec) -> Self {
        Self {
            spec,
            describe: None,
            init: None,
            child: None,
            consecutive_process_failures: 0,
            disabled_failure: None,
        }
    }

    pub const fn describe_info(&self) -> Option<&Describe> {
        self.describe.as_ref()
    }

    /// Run `describe` once per gate per run. Nonzero exit, timeout, extra
    /// stdout, or an invalid object is a startup failure.
    pub async fn describe(&mut self) -> Result<Describe, GateFailure> {
        if let Some(describe) = &self.describe {
            return Ok(describe.clone());
        }
        let mut command = self.base_command("describe");
        command.stdin(Stdio::null());
        let mut child = command.spawn().map_err(|error| {
            GateFailure::config(format!(
                "failed to spawn '{} describe': {error}",
                self.spec.program.display()
            ))
        })?;
        let pgid = child.id();
        let Some(stdout) = child.stdout.take() else {
            terminate_process_group(&mut child, pgid).await;
            return Err(GateFailure::config("describe stdout was not piped"));
        };
        let mut stdout = BufReader::new(stdout);
        let deadline = self.spec.startup_timeout;
        let read = async {
            let frame = read_single_describe_frame(&mut stdout).await?;
            let status = child.wait().await.map_err(|error| {
                GateFailure::crash(format!("failed to wait for 'describe': {error}"))
            })?;
            if !status.success() {
                return Err(GateFailure::crash(format!(
                    "'describe' exited with {status}"
                )));
            }
            Ok(frame)
        };
        match timeout(deadline, read).await {
            Ok(Ok(describe)) => {
                self.describe = Some(describe.clone());
                Ok(describe)
            },
            Ok(Err(failure)) => {
                terminate_process_group(&mut child, pgid).await;
                Err(failure)
            },
            Err(_) => {
                terminate_process_group(&mut child, pgid).await;
                Err(GateFailure::timeout(format!(
                    "'describe' exceeded {}ms",
                    deadline.as_millis()
                )))
            },
        }
    }

    /// Spawn `serve` and complete the `init`/`ready` handshake.
    pub async fn start(&mut self, init: InitFrame) -> Result<ReadyFrame, GateFailure> {
        let Some(describe) = self.describe.clone() else {
            return Err(GateFailure::config("describe must run before start"));
        };
        validate_init(&init)?;
        self.init = Some(init.clone());
        let mut command = self.base_command("serve");
        command.stdin(Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            GateFailure::config(format!(
                "failed to spawn '{} serve': {error}",
                self.spec.program.display()
            ))
        })?;
        let pgid = child.id();
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            terminate_process_group(&mut child, pgid).await;
            return Err(GateFailure::config("serve stdio was not piped"));
        };
        let mut stdin = stdin;
        let mut stdout = BufReader::new(stdout);
        let deadline = self.spec.startup_timeout;
        let handshake = async {
            write_frame(&mut stdin, &init).await?;
            read_ready_frame(&mut stdout, &describe).await
        };
        match timeout(deadline, handshake).await {
            Ok(Ok(ready)) => {
                // A restart's ready handshake does not reset the breaker: a
                // gate that crashes on every check would otherwise restart and
                // handshake forever. A completed check resets the count.
                self.child = Some(GateChild {
                    child,
                    stdin,
                    stdout,
                    pgid,
                });
                Ok(ready)
            },
            Ok(Err(failure)) => {
                terminate_process_group(&mut child, pgid).await;
                Err(failure)
            },
            Err(_) => {
                terminate_process_group(&mut child, pgid).await;
                Err(GateFailure::timeout(format!(
                    "'init' exceeded {}ms",
                    deadline.as_millis()
                )))
            },
        }
    }

    /// Run one check. The process is restarted after a process-level failure;
    /// two consecutive process-level failures disable the gate for the run.
    pub async fn check(&mut self, frame: CheckFrame) -> Result<DecisionFrame, GateCheckFailure> {
        if let Some(failure) = &self.disabled_failure {
            return Err(GateCheckFailure::Disabled {
                failure: failure.clone(),
            });
        }
        if self.child.is_none() {
            let Some(init) = self.init.clone() else {
                return Err(self.process_failure(GateFailure::config(
                    "gate process is not started and has no init frame",
                )));
            };
            if let Err(failure) = self.start(init).await {
                return Err(self.process_failure(failure));
            }
        }
        let Some(child) = self.child.as_mut() else {
            return Err(self.process_failure(GateFailure::config(
                "gate process disappeared before the check",
            )));
        };
        let deadline = self.spec.check_timeout;
        let exchange = async {
            write_frame(&mut child.stdin, &frame).await?;
            read_check_frame(&mut child.stdout, &frame.id).await
        };
        match timeout(deadline, exchange).await {
            Ok(Ok(Ok(decision))) => {
                self.consecutive_process_failures = 0;
                Ok(decision)
            },
            Ok(Ok(Err(failure))) => {
                self.consecutive_process_failures = 0;
                Err(GateCheckFailure::Failure(failure))
            },
            Ok(Err(failure)) => Err(self.process_failure(failure)),
            Err(_) => Err(self.process_failure(GateFailure::timeout(format!(
                "check '{}' exceeded {}ms",
                frame.id,
                deadline.as_millis()
            )))),
        }
    }

    /// Send `shutdown`, close stdin, wait briefly, then terminate the process
    /// group. Shutdown is best effort and never changes an outcome.
    pub async fn shutdown(&mut self) {
        let Some(mut gate_child) = self.child.take() else {
            return;
        };
        let frame = ShutdownFrame::new();
        drop(
            timeout(
                Duration::from_millis(250),
                write_frame(&mut gate_child.stdin, &frame),
            )
            .await,
        );
        drop(gate_child.stdin);
        let direct_exited = timeout(SHUTDOWN_WAIT, gate_child.child.wait())
            .await
            .is_ok();
        // The direct child exiting does not mean the process group is gone: a
        // gate may have spawned descendants. Always signal the group so those
        // descendants cannot outlive the run.
        signal_process_group(gate_child.pgid, SIGTERM);
        if !direct_exited {
            drop(timeout(KILL_WAIT, gate_child.child.wait()).await);
        }
        signal_process_group(gate_child.pgid, SIGKILL);
        drop(timeout(KILL_WAIT, gate_child.child.wait()).await);
    }

    fn base_command(&self, subcommand: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.spec.program);
        command.args(&self.spec.args).arg(subcommand);
        if let Some(cwd) = &self.spec.cwd {
            command.current_dir(cwd);
        }
        command.env_clear();
        for name in BASE_ENV {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        for (name, value) in &self.spec.environment {
            command.env(name, value);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.as_std_mut().process_group(0);
        }
        command
    }

    fn process_failure(&mut self, failure: GateFailure) -> GateCheckFailure {
        self.consecutive_process_failures = self.consecutive_process_failures.saturating_add(1);
        self.kill_child_now();
        if self.consecutive_process_failures >= 2 {
            self.disabled_failure = Some(failure.clone());
        }
        GateCheckFailure::Failure(failure)
    }

    fn kill_child_now(&mut self) {
        if let Some(mut gate_child) = self.child.take() {
            signal_process_group(gate_child.pgid, SIGKILL);
            drop(gate_child.child.start_kill());
            gate_child.pgid = None;
        }
    }
}

impl Drop for GateProcess {
    fn drop(&mut self) {
        self.kill_child_now();
    }
}

fn validate_init(init: &InitFrame) -> Result<(), GateFailure> {
    if init.frame_type != FrameType::Init {
        return Err(GateFailure::config("init frame has the wrong type"));
    }
    if init.id != InitFrame::INIT_ID {
        return Err(GateFailure::config("init frame id must be 'init'"));
    }
    if init.session.cwd.is_empty() {
        return Err(GateFailure::config("init session cwd must not be empty"));
    }
    if init.env.len() > MAX_ENV_NAMES {
        return Err(GateFailure::config(format!(
            "init env has more than {MAX_ENV_NAMES} names"
        )));
    }
    if let Some(config) = &init.config
        && json_byte_len(config) > MAX_INIT_CONFIG_BYTES
    {
        return Err(GateFailure::config(format!(
            "init config exceeds {MAX_INIT_CONFIG_BYTES} bytes"
        )));
    }
    Ok(())
}

async fn read_single_describe_frame(
    stdout: &mut BufReader<tokio::process::ChildStdout>,
) -> Result<Describe, GateFailure> {
    let Some(line) = read_frame_line(stdout)
        .await
        .map_err(FrameReadError::describe_failure)?
    else {
        return Err(GateFailure::config("'describe' wrote no object"));
    };
    let describe: Describe = serde_json::from_str(&line)
        .map_err(|error| GateFailure::config(format!("invalid describe object: {error}")))?;
    describe
        .validate()
        .map_err(|message| GateFailure::config(format!("invalid describe object: {message}")))?;
    let mut extra = [0_u8; 256];
    let extra_bytes = stdout
        .read(&mut extra)
        .await
        .map_err(|error| GateFailure::crash(format!("failed to read describe stdout: {error}")))?;
    if extra_bytes > 0 {
        return Err(GateFailure::config(
            "'describe' wrote extra data after its object",
        ));
    }
    Ok(describe)
}

async fn read_ready_frame(
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    describe: &Describe,
) -> Result<ReadyFrame, GateFailure> {
    let Some(line) = read_frame_line(stdout)
        .await
        .map_err(FrameReadError::startup_failure)?
    else {
        return Err(GateFailure::crash("gate closed stdout before 'ready'"));
    };
    let value: serde_json::Value = serde_json::from_str(&line)
        .map_err(|error| GateFailure::protocol(format!("invalid init reply: {error}")))?;
    let Some(frame_type) = value.get("type").and_then(serde_json::Value::as_str) else {
        return Err(GateFailure::protocol("init reply has no 'type' field"));
    };
    match frame_type {
        "ready" => parse_ready_frame(value, describe),
        "error" => Err(parse_init_error(value)?),
        other => Err(GateFailure::protocol(format!(
            "unexpected init reply type '{other}'"
        ))),
    }
}

fn parse_ready_frame(
    value: serde_json::Value,
    describe: &Describe,
) -> Result<ReadyFrame, GateFailure> {
    let ready: ReadyFrame = serde_json::from_value(value)
        .map_err(|error| GateFailure::protocol(format!("invalid ready frame: {error}")))?;
    if ready.protocol != PROTOCOL_VERSION {
        return Err(GateFailure::protocol(format!(
            "ready speaks '{}', expected '{PROTOCOL_VERSION}'",
            ready.protocol
        )));
    }
    if ready.frame_type != FrameType::Ready {
        return Err(GateFailure::protocol("init reply is not a ready frame"));
    }
    if ready.id != InitFrame::INIT_ID {
        return Err(GateFailure::protocol(format!(
            "ready id '{}' does not match 'init'",
            ready.id
        )));
    }
    if ready.name != describe.name || ready.version != describe.version {
        return Err(GateFailure::protocol(
            "ready name or version does not match describe",
        ));
    }
    let mut ready = ready;
    ready.warnings = ready
        .warnings
        .iter()
        .take(MAX_READY_WARNINGS)
        .map(|warning| bound_chars(&sanitize_control_text(warning), 1_024))
        .collect();
    Ok(ready)
}

fn parse_init_error(value: serde_json::Value) -> Result<GateFailure, GateFailure> {
    let error: ErrorFrame = serde_json::from_value(value)
        .map_err(|error| GateFailure::protocol(format!("invalid error frame: {error}")))?;
    validate_error_header(&error, InitFrame::INIT_ID)?;
    Ok(GateFailure::new(
        error.class.normalized(),
        format!("gate failed init: {}", bound_error_message(&error.message)),
    ))
}

fn validate_error_header(error: &ErrorFrame, expected_id: &str) -> Result<(), GateFailure> {
    if error.protocol != PROTOCOL_VERSION {
        return Err(GateFailure::protocol(format!(
            "error speaks '{}', expected '{PROTOCOL_VERSION}'",
            error.protocol
        )));
    }
    if error.frame_type != FrameType::Error {
        return Err(GateFailure::protocol("error frame has the wrong type"));
    }
    if error.id != expected_id {
        return Err(GateFailure::protocol(format!(
            "error id '{}' does not match request '{expected_id}'",
            error.id
        )));
    }
    Ok(())
}

async fn read_check_frame(
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    expected_id: &str,
) -> Result<Result<DecisionFrame, GateFailure>, GateFailure> {
    let Some(line) = read_frame_line(stdout)
        .await
        .map_err(FrameReadError::check_failure)?
    else {
        return Err(GateFailure::crash("gate closed stdout during a check"));
    };
    let value: serde_json::Value = serde_json::from_str(&line)
        .map_err(|error| GateFailure::protocol(format!("invalid frame: {error}")))?;
    let Some(frame_type) = value.get("type").and_then(serde_json::Value::as_str) else {
        return Err(GateFailure::protocol("frame has no 'type' field"));
    };
    match frame_type {
        "decision" => {
            let decision: DecisionFrame = serde_json::from_value(value).map_err(|error| {
                GateFailure::protocol(format!("invalid decision frame: {error}"))
            })?;
            decision
                .validate(expected_id)
                .map_err(GateFailure::protocol)?;
            Ok(Ok(decision))
        },
        "error" => {
            let error: ErrorFrame = serde_json::from_value(value)
                .map_err(|error| GateFailure::protocol(format!("invalid error frame: {error}")))?;
            validate_error_header(&error, expected_id)?;
            Ok(Err(GateFailure::new(
                error.class.normalized(),
                bound_error_message(&error.message),
            )))
        },
        other => Err(GateFailure::protocol(format!(
            "unexpected frame type '{other}'"
        ))),
    }
}

async fn write_frame<T: Serialize + Sync>(
    stdin: &mut ChildStdin,
    frame: &T,
) -> Result<(), GateFailure> {
    let mut bytes = serde_json::to_vec(frame)
        .map_err(|error| GateFailure::new(GateErrorClass::Internal, error.to_string()))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(GateFailure::protocol("outbound frame exceeds 1 MiB"));
    }
    bytes.push(b'\n');
    stdin
        .write_all(&bytes)
        .await
        .map_err(|error| GateFailure::crash(format!("failed to write frame: {error}")))?;
    stdin
        .flush()
        .await
        .map_err(|error| GateFailure::crash(format!("failed to flush frame: {error}")))
}

enum FrameReadError {
    Io(std::io::Error),
    EofAfterPartial,
    TooLarge,
    InvalidUtf8,
}

impl FrameReadError {
    fn describe_failure(self) -> GateFailure {
        match self {
            Self::Io(error) => {
                GateFailure::crash(format!("failed to read describe stdout: {error}"))
            },
            Self::EofAfterPartial => GateFailure::config("describe wrote an unterminated frame"),
            Self::TooLarge => GateFailure::config("describe frame exceeds 1 MiB"),
            Self::InvalidUtf8 => GateFailure::config("describe frame is not UTF-8"),
        }
    }

    fn startup_failure(self) -> GateFailure {
        match self {
            Self::Io(error) => GateFailure::crash(format!("failed to read init reply: {error}")),
            Self::EofAfterPartial => GateFailure::protocol("gate wrote an unterminated frame"),
            Self::TooLarge => GateFailure::protocol("init frame exceeds 1 MiB"),
            Self::InvalidUtf8 => GateFailure::protocol("init frame is not UTF-8"),
        }
    }

    fn check_failure(self) -> GateFailure {
        match self {
            Self::Io(error) => GateFailure::crash(format!("failed to read gate frame: {error}")),
            Self::EofAfterPartial => GateFailure::protocol("gate wrote an unterminated frame"),
            Self::TooLarge => GateFailure::protocol("gate frame exceeds 1 MiB"),
            Self::InvalidUtf8 => GateFailure::protocol("gate frame is not UTF-8"),
        }
    }
}

async fn read_frame_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<String>, FrameReadError> {
    let mut bytes = Vec::new();
    loop {
        let chunk = reader.fill_buf().await.map_err(FrameReadError::Io)?;
        if chunk.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            return Err(FrameReadError::EofAfterPartial);
        }
        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(chunk.len());
        if bytes.len().saturating_add(take) > MAX_FRAME_BYTES {
            return Err(FrameReadError::TooLarge);
        }
        bytes.extend_from_slice(&chunk[..take]);
        reader.consume(take + usize::from(newline.is_some()));
        if newline.is_some() {
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|_utf8_error| FrameReadError::InvalidUtf8);
        }
    }
}

async fn terminate_process_group(child: &mut Child, pgid: Option<u32>) {
    signal_process_group(pgid, SIGTERM);
    drop(timeout(KILL_WAIT, child.wait()).await);
    // The direct child may exit while a descendant keeps the group alive. The
    // forceful phase always follows the cooperative one, independent of the
    // direct child's exit status.
    signal_process_group(pgid, SIGKILL);
    drop(timeout(KILL_WAIT, child.wait()).await);
}

#[cfg(unix)]
fn signal_process_group(pgid: Option<u32>, signal: libc::c_int) {
    if let Some(pid) = pgid {
        // SAFETY: the child was spawned with `process_group(0)`, so its process
        // group id equals the pid captured while it was alive. A negative pid
        // in kill(2) targets that whole group.
        unsafe {
            libc::kill(-(pid as libc::pid_t), signal);
        }
    }
}

#[cfg(not(unix))]
fn signal_process_group(_pgid: Option<u32>, _signal: i32) {}

/// A gate backed by a `serve` child. Startup is explicit because `describe`
/// and `init` are async and must fail the pipeline before any check runs.
#[derive(Debug)]
pub struct ProcessGate {
    describe: Describe,
    process: tokio::sync::Mutex<GateProcess>,
}

impl ProcessGate {
    /// Run `describe` and `init` now. An error is an enforce pipeline failure or
    /// a shadow gate that the caller records and drops.
    pub async fn start(init: InitFrame, mut process: GateProcess) -> Result<Self, GateFailure> {
        let describe = process.describe().await?;
        let _ready = process.start(init).await?;
        Ok(Self {
            describe,
            process: tokio::sync::Mutex::new(process),
        })
    }
}

impl crate::gates::composer::Gate for ProcessGate {
    fn describe(&self) -> &Describe {
        &self.describe
    }

    fn check(&self, frame: CheckFrame) -> crate::gates::composer::GateFuture<'_> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            let mut process = self.process.lock().await;
            match process.check(frame).await {
                Ok(decision) => crate::gates::composer::GateAnswer::decision(
                    decision,
                    started.elapsed(),
                    None,
                    false,
                    Vec::new(),
                ),
                Err(failure) => crate::gates::composer::GateAnswer::failure(
                    failure.failure().clone(),
                    failure.is_disabled(),
                    started.elapsed(),
                    Vec::new(),
                ),
            }
        })
    }

    fn shutdown(&self) -> crate::gates::composer::GateShutdownFuture<'_> {
        Box::pin(async move {
            self.process.lock().await.shutdown().await;
        })
    }
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
