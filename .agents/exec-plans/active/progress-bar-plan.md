# Plan: OSC 9;4 Terminal Progress Indicator

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agents/PLANS.md` from the repository root. It was migrated from the former `.agents/.plans/` location and remains active because the current codebase has an `indicatif` text spinner but does not contain the OSC 9;4 terminal progress indicator described by this plan.

## Purpose / Big Picture

Cake should set a terminal tab or taskbar progress indicator while the model is working, in terminals that support the OSC 9;4 sequence. After this work, a user running cake in a compatible interactive terminal will see an indeterminate progress indicator start during generation and clear when the task finishes or errors, without polluting JSON or redirected output.

The behavior is observable by running cake from an interactive terminal that supports OSC 9;4 and by running unit tests that prove the exact escape sequences are formatted correctly.

## Progress

- [x] (2026-05-07 18:46Z) Confirmed the current codebase uses `indicatif` for an in-terminal text spinner.
- [x] (2026-05-07 18:46Z) Confirmed there is no `src/progress.rs` module and no OSC 9;4 sequence implementation in the current tree.
- [x] (2026-05-07 18:46Z) Migrated this plan to `.agents/exec-plans/active/progress-bar-plan.md` and added the required ExecPlan lifecycle sections.
- [ ] Implement terminal-gated OSC 9;4 progress helpers.
- [ ] Wire start and stop behavior into the run loop with cleanup on error.
- [ ] Add tests, documentation if needed, and run `just ci`.

## Surprises & Discoveries

- Observation: Cake already has human-readable progress messages in text mode.
  Evidence: `src/main.rs::with_text_progress` configures an `indicatif::ProgressBar`, while `src/clients/agent.rs` exposes a progress callback.

- Observation: The existing spinner is not the OSC feature described here.
  Evidence: `rg -n "OSC 9;4|\\]9;4|src/progress" src docs README.md` finds no implementation.

## Decision Log

- Decision: Classify this plan as active during the ExecPlan migration.
  Rationale: The user-visible text spinner exists, but the specific terminal progress sequence described by the plan has not been implemented.
  Date/Author: 2026-05-07 / Codex

## Outcomes & Retrospective

No OSC 9;4 implementation has started. At completion, update this section with the final lifecycle integration, test results, and whether stdout or stderr was used for the escape sequence after validating JSON-output behavior.

## Background

The ConEmu "Progress Bar" escape sequence spec (documented by Microsoft at https://learn.microsoft.com/en-us/windows/terminal/tutorials/progress-bar-sequences) defines OSC escape sequences that let a CLI set a progress indicator on the terminal tab or taskbar. Supported terminals include iTerm2, Windows Terminal, WezTerm, and others that implement this spec.

The OSC 9;4 sequence format is:

```
ESC ] 9 ; 4 ; <state> ; <progress> BEL
```

Where `<state>` is:
- `0` = remove/hide the progress indicator
- `1` = set progress value (0-100) determinate
- `2` = set progress value (0-100) in error state
- `3` = set indeterminate (spinning) state
- `4` = set indeterminate state in warning/paused state

And `<progress>` is an integer 0-100 (only meaningful for states 1 and 2).

cake implements only states 0 (off) and 3 (indeterminate). This plan keeps that same scope.

## Goal

Add OSC 9;4 progress indication to cake so that compatible terminals show a spinning/indeterminate progress indicator on the tab while the LLM is generating a response. The indicator appears when streaming starts and disappears when streaming ends or an error occurs.

## Implementation

### 1. Create `src/progress.rs` module

Add a new module with a small API for writing OSC 9;4 sequences to stdout:

```rust
/// State values for OSC 9;4 progress sequences.
mod state {
    pub const OFF: u8 = 0;
    pub const INDETERMINATE: u8 = 3;
}

/// Write an OSC 9;4 sequence to stdout.
///
/// Format: `\x1b]9;4;{state};{progress}\x07`
///
/// This is a no-op when stdout is not a terminal (piped, redirected, etc.)
/// so that JSON streaming and other non-interactive modes stay clean.
fn write_osc_progress(state: u8, progress: u8) {
    if std::io::stdout().is_terminal() {
        // Use a direct write to stdout to avoid buffering issues.
        // The sequence must reach the terminal immediately.
        let seq = format!("\x1b]9;4;{state};{progress}\x07");
        let _ = std::io::Write::write_all(&mut std::io::stdout(), seq.as_bytes());
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
}

/// Show an indeterminate progress indicator on the terminal tab/taskbar.
pub fn start_progress() {
    write_osc_progress(state::INDETERMINATE, 0);
}

/// Hide the progress indicator on the terminal tab/taskbar.
pub fn stop_progress() {
    write_osc_progress(state::OFF, 0);
}
```

Key design decisions:
- **Terminal check**: Only write the sequence when stdout is a real terminal. This prevents escape sequences from leaking into piped output (stream-json mode, redirects, etc.).
- **Direct write**: Use `std::io::Write` directly on stdout rather than `print!`/`println!` because the Rust stdout buffer may not flush at the right time, and we want the sequence sent immediately.
- **No progress value**: Only states 0 and 3 are used, so the progress field is always 0.

### 2. Register the module in `main.rs`

Add `mod progress;` to the module declarations in `src/main.rs`.

### 3. Call `start_progress()` and `stop_progress()` from the run loop

In `CodingAssistant::run()` (the `CmdRunner` impl in `src/main.rs`):

- Call `progress::start_progress()` right before `client.send(msg).await` (after the spinner is set up, around line 440).
- Call `progress::stop_progress()` in the cleanup path after `client.send()` returns, regardless of success or error. This goes alongside the existing spinner cleanup code (around line 470).

The placement ensures the OSC sequence is sent at the same lifecycle points as the `indicatif` spinner: visible while the agent is working, gone when it finishes.

### 4. Handle error paths

`stop_progress()` must be called even if `client.send()` returns an error. Wrap the call in a `Drop` guard or call it explicitly after the result match. A `Drop` guard is cleaner and avoids forgetting on early returns:

```rust
struct ProgressGuard;

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        progress::stop_progress();
    }
}
```

Create the guard before calling `client.send()`. When the guard goes out of scope (whether on success or error), `stop_progress()` fires automatically.

### 5. Add unit tests in `src/progress.rs`

- Test that `write_osc_progress` produces the correct byte sequence for states 0 and 3.
- Test that `start_progress()` and `stop_progress()` produce the expected sequences.

Since these functions write directly to stdout, the tests will verify the format string construction rather than mocking stdout. Extract the sequence formatting into a testable helper:

```rust
fn format_osc_progress(state: u8, progress: u8) -> String {
    format!("\x1b]9;4;{state};{progress}\x07")
}
```

Then `write_osc_progress` calls this and writes the result. Tests assert on the formatted string.

### 6. Integration with existing spinner

The OSC progress indicator and the `indicatif` spinner are independent mechanisms that work in parallel:
- **OSC 9;4**: Sets progress on the terminal tab/taskbar icon. Invisible in the terminal content area.
- **indicatif spinner**: Shows a spinning animation with status text in the terminal content area.

No changes needed to the spinner code. Both start at the same time and both stop at the same time. The `Verbosity::Quiet` case already skips spinner setup, and since `stop_progress()` writes an "off" sequence, calling it when progress was never started is harmless (the terminal ignores it).

### 7. Stream-JSON mode behavior

In stream-JSON mode, `Verbosity` is set to `Quiet`, which means stdout is being used for JSON output. The `is_terminal()` check in `write_osc_progress` ensures no escape sequences are written when stdout is piped or redirected, which covers the stream-JSON use case (where cake's stdout is typically consumed by another program). No additional gating is needed.

## File Changes Summary

| File | Change |
|------|--------|
| `src/progress.rs` | New file. OSC 9;4 sequence formatting and terminal-gated write functions. `ProgressGuard` Drop type. Unit tests. |
| `src/main.rs` | Add `mod progress;`. Create `ProgressGuard` before `client.send()`. |
| `Cargo.toml` | No changes needed. `std::io::stdout().is_terminal()` is stable in Rust 1.70+ and requires no additional dependency. |

## Non-Goals

- **Determinate progress (states 1, 2, 4)**: Not implemented. We do not have a reliable way to compute percentage progress from the LLM response. Indeterminate only.
- **Terminal detection/heuristic**: We only check `is_terminal()`. We do not try to query the terminal's OSC support via DA1/DA2 responses.
- **Replacing indicatif**: The spinner stays. OSC 9;4 is additive, not a replacement.

## Revision Notes

- 2026-05-07 / Codex: Migrated this historical plan into the new active ExecPlan directory and added lifecycle sections required by `.agents/PLANS.md`. The original design above remains as the implementation starting point.
