use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Maximum number of paths whose last Read is retained for a run.
///
/// The tracker lives for one CLI invocation. A hard cap keeps a long run
/// that reads many files from retaining every observation until exit; the
/// oldest observation is dropped first.
const MAX_TRACKED_PATHS: usize = 128;

/// What one successful Read delivered, plus the truncation signals Edit uses
/// to explain a `no_match` failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadObservation {
    /// 1-indexed first line requested.
    pub start_line: usize,
    /// 1-indexed last line delivered. `None` when no line was delivered.
    pub end_line: Option<usize>,
    /// Number of lines counted. A lower bound when `total_exact` is false.
    pub total_lines: usize,
    /// Whether `total_lines` is the exact file length rather than a lower bound.
    pub total_exact: bool,
    /// Whether the output byte cap stopped delivery before the requested
    /// range was satisfied.
    pub output_truncated: bool,
    /// 1-indexed delivered lines cut at the per-line byte cap.
    pub truncated_lines: Vec<usize>,
}

impl ReadObservation {
    /// Whether the Read covered the whole file. A partial read is the only
    /// case where "`old_text` may be outside the range you read" is plausible.
    pub const fn is_partial(&self) -> bool {
        if self.total_lines == 0 && self.total_exact {
            return false;
        }
        self.start_line > 1
            || self.output_truncated
            || !matches!(
                self.end_line,
                Some(end) if self.total_exact && end >= self.total_lines
            )
    }
}

/// The last Read for one path and whether a later successful Edit invalidated
/// it.
#[derive(Debug, Clone)]
pub struct ReadStatus {
    pub observation: ReadObservation,
    /// True when no Read has happened since the last successful Edit of this
    /// path in the current run.
    pub stale_after_edit: bool,
}

#[derive(Debug)]
struct TrackedRead {
    observation: ReadObservation,
    /// Per-path count of successful Edits observed after the first Read.
    mutation_generation: u64,
    /// The mutation generation at which this observation was recorded.
    read_generation: u64,
    /// Monotonic access counter used to evict the least recently used path.
    last_used: u64,
}

#[derive(Debug, Default)]
struct TrackerState {
    entries: HashMap<PathBuf, TrackedRead>,
    clock: u64,
}

/// Per-run file-read history shared by the Read and Edit tools.
#[derive(Debug, Default)]
pub struct ReadTracker {
    state: Mutex<TrackerState>,
}

impl ReadTracker {
    fn lock(&self) -> MutexGuard<'_, TrackerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record the latest successful Read for `path`, replacing any earlier
    /// observation and marking it fresh relative to prior Edits.
    pub fn record_read(&self, path: &Path, observation: ReadObservation) {
        let mut state = self.lock();
        state.clock = state.clock.saturating_add(1);
        let now = state.clock;

        if let Some(entry) = state.entries.get_mut(path) {
            entry.observation = observation;
            entry.read_generation = entry.mutation_generation;
            entry.last_used = now;
            return;
        }

        if state.entries.len() >= MAX_TRACKED_PATHS
            && let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(path, _)| path.clone())
        {
            state.entries.remove(&oldest);
        }

        state.entries.insert(
            path.to_path_buf(),
            TrackedRead {
                observation,
                mutation_generation: 0,
                read_generation: 0,
                last_used: now,
            },
        );
    }

    /// Record a successful Edit so the next Edit can warn when no Read has
    /// happened since it.
    pub fn record_edit(&self, path: &Path) {
        let mut state = self.lock();
        state.clock = state.clock.saturating_add(1);
        let now = state.clock;

        if let Some(entry) = state.entries.get_mut(path) {
            entry.mutation_generation = entry.mutation_generation.saturating_add(1);
            entry.last_used = now;
        }
    }

    /// Return the latest observation for `path` and whether an Edit has
    /// invalidated it. Returns `None` when this run has not Read the path.
    pub fn status(&self, path: &Path) -> Option<ReadStatus> {
        let mut state = self.lock();
        state.clock = state.clock.saturating_add(1);
        let now = state.clock;

        let entry = state.entries.get_mut(path)?;
        entry.last_used = now;
        let status = ReadStatus {
            observation: entry.observation.clone(),
            stale_after_edit: entry.read_generation < entry.mutation_generation,
        };
        drop(state);
        Some(status)
    }
}
