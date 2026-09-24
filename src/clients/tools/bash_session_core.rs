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
    mut guard: ToolboxProcessGuard,
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
            // Reap only after the pipes close. A descendant can hold a pipe
            // after the shell exits; until then kill/wall needs Child::id().
            result = child.wait(), if capture_finished => {
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
    if !capture_finished {
        drop(tokio::time::timeout(CAPTURE_DRAIN_TIMEOUT, &mut capture).await);
        capture.abort();
    }
    if termination.is_none() {
        guard.defuse();
    }
    drop(guard);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::process::Command;

    fn registry(live_max: usize, output_max_bytes: usize) -> SessionRegistry {
        SessionRegistry::new(live_max, output_max_bytes, Duration::from_secs(10))
    }

    #[test]
    fn journal_reports_exact_gap_and_preserves_utf8_boundary() {
        let now = Instant::now();
        let sessions = registry(1, 3);
        let id = sessions.start("printf text".into(), now).unwrap();
        sessions.append(&id, "abéxy".as_bytes(), false);
        let read = sessions.read(&id, now).unwrap();
        assert_eq!(read.output.output, "xy");
        assert_eq!(read.output.dropped_bytes, 4);
        assert_eq!(read.output.cursor, 6);
        assert_eq!(sessions.read(&id, now).unwrap().output.output, "");
    }

    #[test]
    fn journal_waits_for_split_utf8_character() {
        let now = Instant::now();
        let sessions = registry(1, 100);
        let id = sessions.start("printf unicode".into(), now).unwrap();
        let bytes = "é".as_bytes();
        sessions.append(&id, &bytes[..1], false);
        assert_eq!(sessions.read(&id, now).unwrap().output.output, "");
        sessions.append(&id, &bytes[1..], false);
        assert_eq!(sessions.read(&id, now).unwrap().output.output, "é");
    }

    #[test]
    fn journal_normalizes_invalid_bytes_without_exceeding_raw_cap() {
        let now = Instant::now();
        let sessions = registry(1, 2);
        let id = sessions.start("binary".into(), now).unwrap();
        sessions.append(&id, &[b'a', 0xff, b'b'], false);
        let read = sessions.read(&id, now).unwrap();
        assert_eq!(read.output.output, "�b");
        assert_eq!(read.output.dropped_bytes, 1);
    }

    #[test]
    fn journal_keeps_invalid_continuation_bytes_without_overflow() {
        let now = Instant::now();
        let sessions = registry(1, 2);
        let id = sessions.start("binary".into(), now).unwrap();
        sessions.append(&id, &[0x80], false);
        let read = sessions.read(&id, now).unwrap();
        assert_eq!(read.output.output, "�");
        assert_eq!(read.output.dropped_bytes, 0);

        sessions.append(&id, &[b'a', 0x80, b'b'], false);
        let read = sessions.read(&id, now).unwrap();
        assert_eq!(read.output.output, "�b");
        assert_eq!(read.output.dropped_bytes, 1);
    }

    #[test]
    fn journal_keeps_partial_utf8_after_invalid_byte() {
        let now = Instant::now();
        let sessions = registry(1, 100);
        let id = sessions.start("mixed".into(), now).unwrap();
        sessions.append(&id, &[0xff, "é".as_bytes()[0]], false);
        assert_eq!(sessions.read(&id, now).unwrap().output.output, "�");
        sessions.append(&id, &"é".as_bytes()[1..], false);
        assert_eq!(sessions.read(&id, now).unwrap().output.output, "é");
    }

    #[test]
    fn incremental_reads_and_repeat_final_read() {
        let now = Instant::now();
        let sessions = registry(1, 100);
        let id = sessions.start("printf hello".into(), now).unwrap();
        sessions.append(&id, b"hel", false);
        assert_eq!(sessions.read(&id, now).unwrap().output.output, "hel");
        sessions.append(&id, b"lo", false);
        sessions.finish(&id, 0, None, now);
        let final_read = sessions.read(&id, now).unwrap();
        assert_eq!(final_read.output.output, "lo");
        assert_eq!(final_read.exit_code, Some(0));
        assert_eq!(sessions.read(&id, now).unwrap(), final_read);
    }

    #[test]
    fn live_cap_and_exited_retention_are_independent() {
        let now = Instant::now();
        let sessions = registry(1, 100);
        let first = sessions.start("first".into(), now).unwrap();
        assert!(
            sessions
                .start("second".into(), now)
                .unwrap_err()
                .contains(&first)
        );
        sessions.finish(&first, 1, None, now);
        let second = sessions.start("second".into(), now).unwrap();
        assert_eq!(sessions.list(now).len(), 2);
        assert!(
            sessions
                .read(&first, now + Duration::from_secs(10))
                .is_err()
        );
        assert_eq!(sessions.list(now + Duration::from_secs(10))[0].id, second);
    }

    #[test]
    fn exited_retention_evicts_oldest_without_displacing_live_sessions() {
        let now = Instant::now();
        let sessions = registry(1, 100);
        let live = sessions.start("live".into(), now).unwrap();
        sessions.finish(&live, 0, None, now);
        let mut exited = vec![live];
        for index in 1..=EXITED_SESSIONS_PER_LIVE_SLOT {
            let at = now + Duration::from_secs(index as u64);
            let id = sessions.start(format!("short {index}"), at).unwrap();
            sessions.finish(&id, 0, None, at);
            exited.push(id);
        }
        assert_eq!(sessions.list(now + Duration::from_secs(4)).len(), 4);
        assert!(
            sessions
                .read(&exited[0], now + Duration::from_secs(4))
                .is_err()
        );
        assert_eq!(
            sessions
                .read(&exited[4], now + Duration::from_secs(4))
                .unwrap()
                .exit_code,
            Some(0)
        );
        let active = sessions
            .start("still running".into(), now + Duration::from_secs(4))
            .unwrap();
        assert!(
            sessions
                .list(now + Duration::from_secs(4))
                .iter()
                .any(|s| s.id == active)
        );
    }

    #[test]
    fn clones_share_sessions_and_unknown_ids_name_active_ones() {
        let now = Instant::now();
        let sessions = registry(1, 100);
        let id = sessions.start("sleep 1".into(), now).unwrap();
        let other = sessions.clone();
        assert_eq!(sessions.list(now).len(), 1);
        let error = other.read("bash_missing", now).unwrap_err();
        assert!(error.contains(&id));
        assert!(error.contains("do not survive Cake restarts"));
        assert_eq!(other.list(now)[0].id, id);
    }

    #[test]
    fn discarded_reservation_frees_live_slot() {
        let now = Instant::now();
        let sessions = registry(1, 100);
        let id = sessions.start("never spawned".into(), now).unwrap();
        sessions.discard(&id);
        assert!(sessions.start("next".into(), now).is_ok());
    }

    #[cfg(unix)]
    fn spawn_shell(script: &str) -> Child {
        use std::os::unix::process::CommandExt;

        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        command.spawn().unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_owner_streams_output_and_replays_final_read() {
        let sessions = registry(1, 100);
        let id = sessions
            .start("printf hello".into(), Instant::now())
            .unwrap();
        sessions
            .attach_process(
                &id,
                spawn_shell("printf hello; sleep 0.05; printf world"),
                None,
                Duration::from_secs(2),
            )
            .unwrap();
        let mut output = String::new();
        let final_read = loop {
            let read = sessions
                .read_wait(&id, Duration::from_secs(1))
                .await
                .unwrap();
            output.push_str(&read.output.output);
            if read.exit_code.is_some() {
                break read;
            }
        };
        assert_eq!(output, "helloworld");
        assert_eq!(final_read.exit_code, Some(0));
        assert_eq!(sessions.read(&id, Instant::now()).unwrap(), final_read);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_owner_kill_and_hard_wall_finish_sessions() {
        let sessions = registry(1, 100);
        let id = sessions.start("sleep 10".into(), Instant::now()).unwrap();
        sessions
            .attach_process(
                &id,
                spawn_shell("sleep 10"),
                None,
                Duration::from_millis(50),
            )
            .unwrap();
        let read = sessions
            .read_wait(&id, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(read.exit_code, Some(-1));

        let id = sessions.start("sleep 10".into(), Instant::now()).unwrap();
        sessions
            .attach_process(&id, spawn_shell("sleep 10"), None, Duration::from_secs(5))
            .unwrap();
        sessions.request_kill(&id).unwrap();
        let read = sessions
            .read_wait(&id, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(read.exit_code, Some(-1));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_registry_kills_descendant_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let script = format!(
            "(sleep 1.5; touch '{}') & echo ready; wait",
            marker.display()
        );
        let sessions = registry(1, 100);
        let id = sessions.start(script.clone(), Instant::now()).unwrap();
        sessions
            .attach_process(&id, spawn_shell(&script), None, Duration::from_secs(5))
            .unwrap();
        assert!(
            sessions
                .read_wait(&id, Duration::from_secs(1))
                .await
                .unwrap()
                .output
                .output
                .contains("ready")
        );
        drop(sessions);
        tokio::time::sleep(Duration::from_millis(1700)).await;
        assert!(!marker.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exited_shell_descendant_is_killed_at_wall_or_on_request() {
        for explicit_kill in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let marker = dir.path().join("survived");
            let script = format!("(sleep 2; touch '{}') & exit 0", marker.display());
            let sessions = registry(1, 100);
            let id = sessions.start(script.clone(), Instant::now()).unwrap();
            sessions
                .attach_process(
                    &id,
                    spawn_shell(&script),
                    None,
                    if explicit_kill {
                        Duration::from_secs(5)
                    } else {
                        Duration::from_millis(200)
                    },
                )
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            if explicit_kill {
                sessions.request_kill(&id).unwrap();
            }
            let read = sessions
                .read_wait(&id, Duration::from_secs(1))
                .await
                .unwrap();
            assert_eq!(read.exit_code, Some(-1));
            tokio::time::sleep(Duration::from_millis(2100)).await;
            assert!(
                !marker.exists(),
                "descendant survived explicit_kill={explicit_kill}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_owner_captures_stdout_when_stderr_is_not_piped() {
        use std::os::unix::process::CommandExt;

        let sessions = registry(1, 100);
        let id = sessions
            .start("printf visible".into(), Instant::now())
            .unwrap();
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg("printf visible")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        sessions
            .attach_process(&id, command.spawn().unwrap(), None, Duration::from_secs(2))
            .unwrap();
        let read = sessions
            .read_wait(&id, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(read.output.output, "visible");
    }
}
