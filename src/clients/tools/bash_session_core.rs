//! The bounded session state machine. Kept test-scoped until the Bash worker
//! hands its child and sandbox guard to this registry in the next stage.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const EXITED_SESSIONS_PER_LIVE_SLOT: usize = 4;

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

fn utf8_lead_width(byte: u8) -> Option<usize> {
    match byte {
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

fn valid_utf8_prefix(bytes: &[u8]) -> bool {
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
            inner.prune(now);
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
    fn journal_keeps_invalid_continuation_bytes_without_overflow() {
        let now = Instant::now();
        let sessions = registry(1, 2);
        let id = sessions.start("binary".into(), now).unwrap();
        sessions.append(&id, &[0x80]);
        let read = sessions.read(&id, now).unwrap();
        assert_eq!(read.output.output, "�");
        assert_eq!(read.output.dropped_bytes, 0);

        sessions.append(&id, &[b'a', 0x80, b'b']);
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
    fn exited_retention_evicts_oldest_without_displacing_live_sessions() {
        let now = Instant::now();
        let sessions = registry(1, 100);
        let live = sessions.start("live".into(), now).unwrap();
        sessions.finish(&live, 0, now);
        let mut exited = vec![live];
        for index in 1..=EXITED_SESSIONS_PER_LIVE_SLOT {
            let at = now + Duration::from_secs(index as u64);
            let id = sessions.start(format!("short {index}"), at).unwrap();
            sessions.finish(&id, 0, at);
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
}
