//! The bounded session state machine. Kept test-scoped until the Bash worker
//! hands its child and sandbox guard to this registry in the next stage.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct JournalRead {
    pub output: String,
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
    fn new(max_bytes: usize) -> Self {
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
        while self.bytes.len() > self.max_bytes {
            self.bytes.pop_front();
            self.dropped_bytes += 1;
        }
        // Dropping at a byte limit can land inside a multibyte character.
        // Discard its remaining continuation bytes so the next read starts
        // at a character boundary, and account for every discarded byte.
        while self
            .bytes
            .front()
            .is_some_and(|b| b & 0b1100_0000 == 0b1000_0000)
        {
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
            dropped_bytes: std::mem::take(&mut self.dropped_bytes),
            cursor: self.cursor,
        }
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
    exited: Option<(Instant, i32)>,
    final_read: Option<JournalRead>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SessionRead {
    pub output: JournalRead,
    pub exit_code: Option<i32>,
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
        let mut inner = self.0.lock().unwrap();
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
                exited: None,
                final_read: None,
            },
        );
        drop(inner);
        Ok(id)
    }

    pub fn append(&self, id: &str, chunk: &[u8]) {
        let mut inner = self.0.lock().unwrap();
        if let Some(session) = inner.sessions.get_mut(id)
            && session.exited.is_none()
        {
            session.journal.append(chunk);
        }
    }

    pub fn finish(&self, id: &str, exit_code: i32, now: Instant) {
        let mut inner = self.0.lock().unwrap();
        if let Some(session) = inner.sessions.get_mut(id)
            && session.exited.is_none()
        {
            session.exited = Some((now, exit_code));
        }
    }

    /// Remove a reservation when preflight or spawn fails before the model
    /// receives its ID.
    pub fn discard(&self, id: &str) {
        self.0.lock().unwrap().sessions.remove(id);
    }

    pub fn read(&self, id: &str, now: Instant) -> Result<SessionRead, String> {
        let mut inner = self.0.lock().unwrap();
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
        drop(inner);
        Ok(SessionRead { output, exit_code })
    }

    pub fn list(&self, now: Instant) -> Vec<SessionListing> {
        let mut inner = self.0.lock().unwrap();
        inner.prune(now);
        let mut list: Vec<_> = inner
            .sessions
            .iter()
            .map(|(id, session)| SessionListing {
                id: id.clone(),
                command: session.command.clone(),
                elapsed: now.duration_since(session.started),
                exit_code: session.exited.map(|(_, code)| code),
            })
            .collect();
        drop(inner);
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(live_max: usize, output_max_bytes: usize) -> SessionRegistry {
        SessionRegistry::new(live_max, output_max_bytes, Duration::from_secs(10))
    }

    #[test]
    fn journal_reports_exact_gap_and_preserves_utf8_boundary() {
        let now = Instant::now();
        let sessions = registry(1, 3);
        let id = sessions.start("printf text".into(), now).unwrap();
        sessions.append(&id, "abéxy".as_bytes());
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
        sessions.append(&id, &bytes[..1]);
        assert_eq!(sessions.read(&id, now).unwrap().output.output, "");
        sessions.append(&id, &bytes[1..]);
        assert_eq!(sessions.read(&id, now).unwrap().output.output, "é");
    }

    #[test]
    fn journal_normalizes_invalid_bytes_without_exceeding_raw_cap() {
        let now = Instant::now();
        let sessions = registry(1, 2);
        let id = sessions.start("binary".into(), now).unwrap();
        sessions.append(&id, &[b'a', 0xff, b'b']);
        let read = sessions.read(&id, now).unwrap();
        assert_eq!(read.output.output, "�b");
        assert_eq!(read.output.dropped_bytes, 1);
    }

    #[test]
    fn journal_keeps_partial_utf8_after_invalid_byte() {
        let now = Instant::now();
        let sessions = registry(1, 100);
        let id = sessions.start("mixed".into(), now).unwrap();
        sessions.append(&id, &[0xff, "é".as_bytes()[0]]);
        assert_eq!(sessions.read(&id, now).unwrap().output.output, "�");
        sessions.append(&id, &"é".as_bytes()[1..]);
        assert_eq!(sessions.read(&id, now).unwrap().output.output, "é");
    }

    #[test]
    fn incremental_reads_and_repeat_final_read() {
        let now = Instant::now();
        let sessions = registry(1, 100);
        let id = sessions.start("printf hello".into(), now).unwrap();
        sessions.append(&id, b"hel");
        assert_eq!(sessions.read(&id, now).unwrap().output.output, "hel");
        sessions.append(&id, b"lo");
        sessions.finish(&id, 0, now);
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
        sessions.finish(&first, 1, now);
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
}
