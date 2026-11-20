//! Report cake's run state with the OSC 7501 program status protocol.
//!
//! A terminal that understands the protocol can show what cake is doing without
//! a manifest or a title heuristic: cake reports `working` while the agent loop
//! runs, `done` or `error` when the run ends, `blocked` while it waits for
//! input, and `idle` when the user interrupts. See
//! [Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status).
//!
//! Records go to stderr, only when stderr is a terminal, and only from a run
//! that owns the terminal. Nothing reaches stdout, a session record, a
//! stream-json event, or the model-visible conversation, so machine-readable
//! output and exit behavior are unaffected. Terminals that do not know the
//! sequence ignore it, which is why reporting needs no setting.
//!
//! Reporting is best-effort. A failed write is logged and ignored: a terminal
//! that closed its end must never fail a run. There is no `clear` report,
//! because a process exit already drops a `working` or `blocked` record and a
//! `done` or `error` record is meant to survive it for the user to find.

use std::io::{self, IsTerminal, Write};

use base64::Engine as _;

/// Longest decoded `msg` the protocol accepts, in bytes.
const MAX_MESSAGE_BYTES: usize = 2048;

/// Marks a `msg` that was shortened to fit the protocol's limit.
const ELLIPSIS: &str = "…";

/// What cake reports it is doing.
///
/// The protocol's `clear` is not a state and is not modeled here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// The agent loop is running.
    Working,
    /// The run finished; its result is ready to be seen and survives the exit.
    Done,
    /// The run failed and stopped.
    Error,
    /// The user interrupted the run.
    Idle,
    /// Cake cannot continue until the user provides something.
    Blocked(BlockedKind),
}

/// Why cake is blocked, as the protocol's `kind` values.
///
/// Cake only ever waits for input, so `permission` and `auth` have no trigger
/// yet and are added with the code that reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockedKind {
    /// The user must provide input before cake can continue.
    Question,
}

impl State {
    /// The protocol's `state` value.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Done => "done",
            Self::Error => "error",
            Self::Idle => "idle",
            Self::Blocked(_) => "blocked",
        }
    }

    /// The protocol's `kind` value, reported with `blocked` only.
    const fn kind(self) -> Option<&'static str> {
        match self {
            Self::Blocked(BlockedKind::Question) => Some("question"),
            Self::Working | Self::Done | Self::Error | Self::Idle => None,
        }
    }
}

/// Report `state` for cake's root record on the attached terminal.
///
/// `message` is a one-line human-readable detail, encoded as the protocol's
/// `msg`. Reporting is skipped when stderr is not a terminal, so a piped or
/// redirected stderr never carries a sequence.
pub fn report(state: State, message: Option<&str>) {
    let enabled = io::stderr().is_terminal();
    let mut stderr = io::stderr().lock();
    report_to(&mut stderr, enabled, state, message);
}

/// Write one report to `writer` unless reporting is disabled.
///
/// Extracted from [`report`] so the terminal guard and the emitted bytes stay
/// testable: production passes `stderr` and whether `stderr` is a terminal.
fn report_to<W: Write>(writer: &mut W, enabled: bool, state: State, message: Option<&str>) {
    if !enabled {
        return;
    }

    let record = format_record(state, message);
    if let Err(error) = writer.write_all(record.as_bytes()) {
        tracing::debug!(target: "cake", %error, "Program status was not reported");
    }
}

/// Render one OSC 7501 report for cake's root record.
///
/// `app=cake` identifies the program on every record, and an absent `id`
/// addresses the root record: cake reports one record for itself.
fn format_record(state: State, message: Option<&str>) -> String {
    let mut record = String::from("\x1b]7501;state=");
    record.push_str(state.as_str());
    if let Some(kind) = state.kind() {
        record.push_str(":kind=");
        record.push_str(kind);
    }
    record.push_str(":app=cake");
    if let Some(message) = message.and_then(normalize_message) {
        record.push_str(":msg=");
        record.push_str(&base64::engine::general_purpose::STANDARD.encode(message));
    }
    record.push_str("\x1b\\");
    record
}

/// Reduce `message` to a valid protocol `msg` value.
///
/// A decoded `msg` containing a control character makes the terminal discard
/// the whole report, so every control character and whitespace run collapses to
/// one space, trailing space is dropped, and the result is shortened to the
/// protocol's decoded limit. Returns `None` when nothing is left to report.
fn normalize_message(message: &str) -> Option<String> {
    let mut normalized = String::with_capacity(message.len().min(MAX_MESSAGE_BYTES));
    for character in message.chars() {
        if character.is_whitespace() || is_disallowed_control(character) {
            if !normalized.is_empty() && !normalized.ends_with(' ') {
                normalized.push(' ');
            }
        } else {
            normalized.push(character);
        }
    }

    let normalized = normalized.trim_end();
    if normalized.is_empty() {
        return None;
    }
    Some(truncate_message(normalized))
}

/// Whether a character is one the protocol forbids in decoded free text.
///
/// Whitespace is handled with this class; it adds the remaining control
/// characters, which `char::is_whitespace` does not cover.
const fn is_disallowed_control(character: char) -> bool {
    matches!(character, '\u{0}'..='\u{1f}' | '\u{7f}' | '\u{80}'..='\u{9f}')
}

/// Shorten a message to the protocol's decoded limit without splitting a
/// character, so the encoded value stays within the encoded limit too.
fn truncate_message(message: &str) -> String {
    if message.len() <= MAX_MESSAGE_BYTES {
        return String::from(message);
    }

    let limit = MAX_MESSAGE_BYTES - ELLIPSIS.len();
    let mut truncated = String::with_capacity(MAX_MESSAGE_BYTES);
    for character in message.chars() {
        if truncated.len() + character.len_utf8() > limit {
            break;
        }
        truncated.push(character);
    }
    truncated.push_str(ELLIPSIS);
    truncated
}
