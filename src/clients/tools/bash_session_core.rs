//! Bounded, in-run Bash process ownership and incremental output.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::sync::{Notify, oneshot};

use crate::config::toolbox::ToolboxProcessGuard;

const EXITED_SESSIONS_PER_LIVE_SLOT: usize = 4;
const CAPTURE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct JournalRead {
    pub output: String,
    pub raw: Vec<u8>,
    pub dropped_bytes: usize,
    pub cursor: u64,
}

#[derive(Debug)]
struct Journal {
    bytes: VecDeque<u8>,
    max_bytes: usize,
    dropped_bytes: usize,
    cursor: u64,
}

impl Journal {
    const fn new(max_bytes: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            max_bytes,
            dropped_bytes: 0,
            cursor: 0,
        }
    }

    fn append(&mut self, chunk: &[u8]) {
        self.cursor = self.cursor.saturating_add(chunk.len() as u64);
        self.bytes.extend(chunk);
        let overflow = self.bytes.len().saturating_sub(self.max_bytes);
        let discard = utf8_trim_boundary(&self.bytes, overflow);
        for _ in 0..discard {
            self.bytes.pop_front();
            self.dropped_bytes += 1;
        }
    }

    fn take(&mut self, finalized: bool) -> JournalRead {
        let mut bytes: Vec<_> = self.bytes.drain(..).collect();
        if !finalized && let Some(start) = incomplete_utf8_suffix(&bytes) {
            let suffix = bytes.split_off(start);
            self.bytes.extend(suffix);
        }
        JournalRead {
            output: String::from_utf8_lossy(&bytes).into_owned(),
            raw: bytes,
            dropped_bytes: std::mem::take(&mut self.dropped_bytes),
            cursor: self.cursor,
        }
    }
}

/// Advance a byte-limit cut only when it splits a valid UTF-8 character.
/// Standalone invalid continuation bytes stay in the journal for lossy decode.
fn utf8_trim_boundary(bytes: &VecDeque<u8>, cut: usize) -> usize {
    if cut == 0 {
        return 0;
    }
    for start in cut.saturating_sub(3)..cut {
        let Some(width) = utf8_lead_width(bytes[start]) else {
            continue;
        };
        if start + width <= cut {
            continue;
        }
        let end = (start + width).min(bytes.len());
        let candidate: Vec<_> = bytes.range(start..end).copied().collect();
        if valid_utf8_prefix(&candidate) {
            return end;
        }
    }
    cut
}

const fn utf8_lead_width(byte: u8) -> Option<usize> {
    match byte {
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

const fn valid_utf8_prefix(bytes: &[u8]) -> bool {
    match std::str::from_utf8(bytes) {
        Ok(_) => true,
        Err(error) => error.valid_up_to() == 0 && error.error_len().is_none(),
    }
}

/// Keep a trailing partial UTF-8 character until a later pipe read completes
/// it, even when earlier bytes in the same chunk were malformed.
fn incomplete_utf8_suffix(bytes: &[u8]) -> Option<usize> {
    let first = bytes.len().saturating_sub(3);
    (first..bytes.len()).find(|&start| {
        std::str::from_utf8(&bytes[start..])
            .is_err_and(|error| error.valid_up_to() == 0 && error.error_len().is_none())
    })
}

#[derive(Debug)]
struct Session {
    command: String,
    started: Instant,
    journal: Journal,
    stderr: VecDeque<u8>,
    exited: Option<(Instant, i32)>,
    termination: Option<&'static str>,
    final_read: Option<JournalRead>,
    notify: Arc<Notify>,
    process: Option<ProcessControl>,
}

#[derive(Debug)]
struct ProcessControl {
    abort: tokio::task::AbortHandle,
    kill: Option<oneshot::Sender<()>>,
    shutdown_guard: ToolboxProcessGuard,
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.exited.is_none()
            && let Some(process) = &self.process
        {
            process.abort.abort();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SessionRead {
    pub output: JournalRead,
    pub exit_code: Option<i32>,
    pub stderr: String,
    pub elapsed: Duration,
    pub termination: Option<&'static str>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SessionListing {
    pub id: String,
    pub command: String,
    pub elapsed: Duration,
    pub exit_code: Option<i32>,
}

#[derive(Debug)]
struct RegistryInner {
    sessions: HashMap<String, Session>,
    live_max: usize,
    output_max_bytes: usize,
    exited_ttl: Duration,
}

/// Shared state for in-run sessions. The owner of the actual child process
/// will retain the sandbox and group guards and call `finish` after reaping.
#[derive(Clone, Debug)]
pub(super) struct SessionRegistry(Arc<Mutex<RegistryInner>>);

impl SessionRegistry {
    pub fn new(live_max: usize, output_max_bytes: usize, exited_ttl: Duration) -> Self {
        Self(Arc::new(Mutex::new(RegistryInner {
            sessions: HashMap::new(),
            live_max,
            output_max_bytes,
            exited_ttl,
        })))
    }

    /// Reserve a live slot before preflight or spawn. A full registry does not
    /// evict an older command, even if its output has not been polled.
    pub fn start(&self, command: String, now: Instant) -> Result<String, String> {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.prune(now);
        let mut active = inner.active_ids();
        if active.len() >= inner.live_max {
            active.sort();
            return Err(format!(
                "Bash session limit {} reached; active sessions: {}",
                inner.live_max,
                active.join(", ")
            ));
        }
        let id = format!("bash_{}", uuid::Uuid::new_v4().simple());
        let output_max_bytes = inner.output_max_bytes;
        inner.sessions.insert(
            id.clone(),
            Session {
                command,
                started: now,
                journal: Journal::new(output_max_bytes),
                stderr: VecDeque::new(),
                exited: None,
                termination: None,
                final_read: None,
                notify: Arc::new(Notify::new()),
                process: None,
            },
        );
        drop(inner);
        Ok(id)
    }

    /// Transfer a spawned child and its sandbox resources to the registry.
    /// The worker holds only a weak reference back to the registry, so dropping
    /// the final context aborts every live worker and kills its process group.
    pub fn attach_process(
        &self,
        id: &str,
        child: Child,
        sandbox_guard: Option<super::sandbox::SandboxGuard>,
        hard_wall: Duration,
    ) -> Result<(), String> {
        let child_pid = child.id();
        let guard = ToolboxProcessGuard::new(child_pid);
        let (kill, kill_rx) = oneshot::channel();
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = inner
            .sessions
            .get_mut(id)
            .ok_or_else(|| format!("Bash reservation {id} disappeared"))?;
        if session.process.is_some() || session.exited.is_some() {
            return Err(format!(
                "Bash session {id} is already attached or completed"
            ));
        }
        let deadline = session.started + hard_wall;
        let registry = Arc::downgrade(&self.0);
        let id_owned = id.to_owned();
        let worker = tokio::spawn(async move {
            run_process(
                child,
                guard,
                sandbox_guard,
                registry,
                id_owned,
                deadline,
                kill_rx,
            )
            .await;
        });
        session.process = Some(ProcessControl {
            abort: worker.abort_handle(),
            kill: Some(kill),
            shutdown_guard: ToolboxProcessGuard::new(child_pid),
        });
        drop(inner);
        Ok(())
    }

    pub fn request_kill(&self, id: &str) -> Result<(), String> {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !inner.sessions.contains_key(id) {
            let mut active = inner.active_ids();
            active.sort();
            return Err(format!(
                "Unknown Bash session {id}; sessions do not survive Cake restarts. Active sessions: {}",
                active.join(", ")
            ));
        }
        let session = inner
            .sessions
            .get_mut(id)
            .ok_or_else(|| format!("Unknown Bash session {id}"))?;
        if let Some(kill) = session
            .process
            .as_mut()
            .and_then(|process| process.kill.take())
        {
            kill.send(()).unwrap_or(());
        }
        drop(inner);
        Ok(())
    }

    pub async fn read_wait(&self, id: &str, wait: Duration) -> Result<SessionRead, String> {
        let notify = {
            let inner = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.sessions.get(id).map(|session| session.notify.clone())
        };
        let Some(notify) = notify else {
            return self.read(id, Instant::now());
        };
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let read = self.read(id, Instant::now())?;
        if !read.output.output.is_empty()
            || read.output.dropped_bytes > 0
            || read.exit_code.is_some()
        {
            return Ok(read);
        }
        drop(tokio::time::timeout(wait, notified).await);
        self.read(id, Instant::now())
    }

    /// Wait for completion without consuming output intended for the Bash
    /// result. Output notifications may wake this loop before the deadline.
    pub async fn wait_for_completion(&self, id: &str, wait: Duration) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notify = {
                let inner = self
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let session = inner
                    .sessions
                    .get(id)
                    .ok_or_else(|| format!("Unknown Bash session {id}"))?;
                let done = session.exited.is_some();
                let notify = session.notify.clone();
                drop(inner);
                if done {
                    return Ok(());
                }
                notify
            };
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_exited(id)? {
                return Ok(());
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Ok(());
            }
        }
    }

    fn is_exited(&self, id: &str) -> Result<bool, String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get(id)
            .map(|session| session.exited.is_some())
            .ok_or_else(|| format!("Unknown Bash session {id}"))
    }

    pub fn append(&self, id: &str, chunk: &[u8], is_stderr: bool) {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(session) = inner.sessions.get_mut(id)
            && session.exited.is_none()
        {
            session.journal.append(chunk);
            if is_stderr {
                session.stderr.extend(chunk);
                let excess = session
                    .stderr
                    .len()
                    .saturating_sub(session.journal.max_bytes);
                session.stderr.drain(..excess);
            }
            session.notify.notify_waiters();
        }
    }

    pub fn finish(
        &self,
        id: &str,
        exit_code: i32,
        termination: Option<&'static str>,
        now: Instant,
    ) {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(session) = inner.sessions.get_mut(id)
            && session.exited.is_none()
        {
            session.exited = Some((now, exit_code));
            session.termination = termination;
            if let Some(process) = &mut session.process {
                process.shutdown_guard.defuse();
            }
            session.process = None;
            session.notify.notify_waiters();
            inner.prune(now);
        }
    }

    /// Remove a reservation when preflight or spawn fails before the model
    /// receives its ID.
    pub fn discard(&self, id: &str) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .remove(id);
    }

    pub fn read(&self, id: &str, now: Instant) -> Result<SessionRead, String> {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.prune(now);
        let Some(session) = inner.sessions.get_mut(id) else {
            let mut active = inner.active_ids();
            drop(inner);
            active.sort();
            return Err(format!(
                "Unknown Bash session {id}; sessions do not survive Cake restarts. Active sessions: {}",
                active.join(", ")
            ));
        };
        let output = if session.exited.is_some() {
            session
                .final_read
                .get_or_insert_with(|| session.journal.take(true))
                .clone()
        } else {
            session.journal.take(false)
        };
        let exit_code = session.exited.map(|(_, code)| code);
        let stderr = String::from_utf8_lossy(&session.stderr.iter().copied().collect::<Vec<_>>())
            .into_owned();
        let elapsed = session
            .exited
            .map_or(now, |(finished, _)| finished)
            .duration_since(session.started);
        let termination = session.termination;
        drop(inner);
        Ok(SessionRead {
            output,
            exit_code,
            stderr,
            elapsed,
            termination,
        })
    }

    pub fn list(&self, now: Instant) -> Vec<SessionListing> {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.prune(now);
        let mut list: Vec<_> = inner
            .sessions
            .iter()
            .map(|(id, session)| SessionListing {
                id: id.clone(),
                command: session.command.clone(),
                elapsed: session
                    .exited
                    .map_or(now, |(finished, _)| finished)
                    .duration_since(session.started),
                exit_code: session.exited.map(|(_, code)| code),
            })
            .collect();
        drop(inner);
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }
}

async fn run_process(
    mut child: Child,
    guard: ToolboxProcessGuard,
    sandbox_guard: Option<super::sandbox::SandboxGuard>,
    registry: std::sync::Weak<Mutex<RegistryInner>>,
    id: String,
    deadline: Instant,
    mut kill: oneshot::Receiver<()>,
) {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut capture = tokio::spawn(capture_output(stdout, stderr, registry.clone(), id.clone()));
    let mut capture_finished = false;
    let deadline = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
    tokio::pin!(deadline);
    let (status, termination) = loop {
        tokio::select! {
            result = &mut capture, if !capture_finished => {
                drop(result);
                capture_finished = true;
            },
            // Observe the direct shell independently of pipe EOF. Descendants may
            // inherit a captured pipe and keep it open after the shell exits.
            result = child.wait() => {
                break (result.ok().and_then(|status| status.code()).unwrap_or(-1), None);
            },
            _ = &mut kill => {
                super::bash::terminate_process_group_gracefully(
                    &mut child, super::bash::TERMINATE_GRACE_PERIOD,
                ).await;
                break (-1, Some("killed"));
            },
            () = &mut deadline => {
                super::bash::terminate_process_group_gracefully(
                    &mut child, super::bash::TERMINATE_GRACE_PERIOD,
                ).await;
                break (-1, Some("hard wall clock reached"));
            },
        }
    };
    // The guard retained the process-group ID from before the direct child was
    // reaped. Drop it before draining output so descendants holding a pipe are
    // stopped first, even when the shell itself exited normally.
    drop(guard);
    if !capture_finished {
        drop(tokio::time::timeout(CAPTURE_DRAIN_TIMEOUT, &mut capture).await);
        capture.abort();
    }
    drop(sandbox_guard);
    if let Some(inner) = registry.upgrade() {
        SessionRegistry(inner).finish(&id, status, termination, Instant::now());
    }
}

async fn capture_output(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    registry: std::sync::Weak<Mutex<RegistryInner>>,
    id: String,
) {
    let mut stdout = stdout;
    let mut stderr = stderr;
    let mut stdout_open = stdout.is_some();
    let mut stderr_open = stderr.is_some();
    let mut stdout_buf = [0u8; 8192];
    let mut stderr_buf = [0u8; 8192];
    while stdout_open || stderr_open {
        let chunk = tokio::select! {
            read = async {
                match stdout.as_mut() {
                    Some(pipe) => pipe.read(&mut stdout_buf).await,
                    None => std::future::pending().await,
                }
            }, if stdout_open => {
                match read { Ok(0) | Err(_) => { stdout_open = false; None }, Ok(n) => Some((&stdout_buf[..n], false)) }
            },
            read = async {
                match stderr.as_mut() {
                    Some(pipe) => pipe.read(&mut stderr_buf).await,
                    None => std::future::pending().await,
                }
            }, if stderr_open => {
                match read { Ok(0) | Err(_) => { stderr_open = false; None }, Ok(n) => Some((&stderr_buf[..n], true)) }
            },
        };
        if let Some((chunk, is_stderr)) = chunk {
            let Some(inner) = registry.upgrade() else {
                return;
            };
            SessionRegistry(inner).append(&id, chunk, is_stderr);
        }
    }
}

impl RegistryInner {
    fn active_ids(&self) -> Vec<String> {
        self.sessions
            .iter()
            .filter(|(_, session)| session.exited.is_none())
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn prune(&mut self, now: Instant) {
        self.sessions.retain(|_, session| {
            session
                .exited
                .is_none_or(|(finished, _)| now.duration_since(finished) < self.exited_ttl)
        });
        let mut exited: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|(id, session)| session.exited.map(|(finished, _)| (id.clone(), finished)))
            .collect();
        let excess = exited
            .len()
            .saturating_sub(self.live_max.saturating_mul(EXITED_SESSIONS_PER_LIVE_SLOT));
        if excess > 0 {
            exited.sort_by_key(|(_, finished)| *finished);
            for (id, _) in exited.into_iter().take(excess) {
                self.sessions.remove(&id);
            }
        }
    }
}
