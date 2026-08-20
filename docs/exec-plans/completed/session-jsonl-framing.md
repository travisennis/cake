# Centralize Session JSONL Header And Line Framing

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

Four independent readers each encode session JSONL header extraction and line framing themselves: full load, replay, discovery, and listing. Two edge-case semantics have drifted between them, so the same session tail can parse differently depending on which consumer reads it. This plan extracts one framing codec plus a positioned record iterator so all four consumers agree on leading-empty-record and partial-tail behavior, while leaving each consumer's validation, normalization, skipping, and error handling in place.

After this change, a session file that starts with blank lines or ends with an interrupted partial record is interpreted identically by `cake`'s session load, `cake replay`, latest-session discovery, and session listing.

Resolves issue #275 (audit finding S04-1). Distinct from #102; #102 is not folded in.

## Progress

- [x] (2026-08-19) Read the four consumer sites and their tests.
- [x] (2026-08-19) Created branch `fix/session-jsonl-framing`.
- [x] (2026-08-19) Wrote this ExecPlan.
- [x] (2026-08-19) Add `src/config/session_jsonl.rs` codec and its tests.
- [x] (2026-08-19) Route `Session::load` through the codec.
- [x] (2026-08-19) Route `replay::load_records` through the codec.
- [x] (2026-08-19) Route `data_dir::read_session_header` through the codec.
- [x] (2026-08-19) Route `sessions::read_session_info` through the codec.
- [x] (2026-08-19) Add one leading-empty test per consumer; run `cargo test session`, replay, session-mode tests.
- [x] (2026-08-19) Run `just ci` (passes), fill Outcomes, move plan to completed, open PR.

## Surprises & Discoveries

- Observation: `read_session_info` in `src/cli/sessions.rs` exceeded the per-function cyclomatic budget after routing the header through the framer, and `load_records` regressed CRAP from a new untested read-error branch. Evidence: `just ci` reported "1 over allowed" and "↑ 2 regressed". Both were fixed by splitting the header parsing into `read_header` (matching the baseline) and by covering the read-error branch in replay.
- Observation: On macOS, `File::open` on a directory succeeds but `read_line` fails with `ErrorKind::IsADirectory`, which cleanly exercises replay's mid-read `Corrupt` mapping in a unit test. Evidence: a small probe binary returned `ERR kind=IsADirectory`.

## Decision Log

- Decision: Place the codec in a new module `src/config/session_jsonl.rs` rather than inside `session.rs`. Rationale: session.rs is already 855 lines and the specifics of framing are a distinct concern that all four consumers share. Date/Author: 2026-08-19 Travis.
- Decision: The codec is streaming over `BufRead` rather than whole-file `String`. Rationale: discovery and listing must stay lightweight (acceptance requirement: no full loads), and `BufReader::read_line` advances one physical line at a time. Full-load consumers wrap their `File` in a `BufReader` the same way. Date/Author: 2026-08-19 Travis.
- Decision: The codec exposes a `partial_tail` flag per line and lets each consumer decide how to handle malformed tails. Rationale: the issue requires malformed-tail *handling* stay outside the codec; framing (knowing a line is a partial tail) stays in it. Date/Author: 2026-08-19 Travis.
- Decision: The codec skips empty and whitespace-only lines uniformly, including leading blank lines before the header. Rationale: this is the drift being fixed; full load and replay already skipped them, discovery and listing did not. Date/Author: 2026-08-19 Travis.
- Decision: First framed line is the header; the codec does not itself validate that it is `session_meta`. Rationale: header validation is consumer-specific and stays out of the codec. Date/Author: 2026-08-19 Travis.
- Decision: Keep replay streaming over a real file rather than reading it whole into a `Cursor`. Rationale: an in-memory cursor forces an infallible read path that clippy rejects (`expect` on `Result`) or adds a dead error branch; streaming keeps replay consistent with the other consumers and lets a unit test (opening a directory) exercise the read-error branch. Date/Author: 2026-08-19 Travis.

## Context and Orientation

Session files are append-only JSONL at `{sessions_dir}/{uuid}.jsonl`. Line one is a `session_meta` record carrying `format_version`, `session_id`, `working_directory`, `timestamp`, and other fields. Each following line is a `SessionRecord` (message, task boundary, tool call, usage, and so on). Records are written via `crate::config::session::Session::append_record`, which serializes one JSON value per line followed by a newline.

Four readers today re-encode framing independently:

- `src/config/session.rs` `Session::load` (full load; reads whole file, skips empty lines, tolerates a partial final line without a trailing newline).
- `src/cli/replay.rs` `load_records` (read-only load; same framing and partial-tail tolerance, maps failures to `ReplayError`).
- `src/config/data_dir.rs` `read_session_header` (latest-session discovery; reads only the first line, parses a minimal header struct, checks version).
- `src/cli/sessions.rs` `read_session_info` (session listing; reads first line, then scans later lines for the first user prompt).

The drift: `Session::load` and replay skip leading empty lines; discovery and listing read the literal first line and error if it is blank. Partial-tail tolerance exists in `Session::load` and replay but not as a shared concept.

Terms of art: "framing" means splitting a file into trimmed, non-empty record lines plus the header. A "partial tail" is a final physical line not terminated by a newline, left by an interrupted writer; consumers optionally tolerate it.

## Plan of Work

1. Create `src/config/session_jsonl.rs` with `SessionFramer<R: BufRead>` and `FramedSessionLine` (fields `line_number: usize` (1-based physical), `text: String` (trimmed), `partial_tail: bool`). `next_record()` reads the next non-empty trimmed line, tracking a 1-based physical line number, and sets `partial_tail` true when the line is final and lacks a trailing newline. Add unit tests. Declare `pub mod session_jsonl;` in `src/config/mod.rs`.

2. Rewrite `Session::load` in `src/config/session.rs` to open the file, wrap it in a `BufReader`, and drive the framer. Keep every header check and error message and the per-record `normalize_legacy_fields` call unchanged. On a parse error where `line.partial_tail` is true, warn and break; otherwise return the existing context error using `line.line_number`.

3. Rewrite `replay::load_records` in `src/cli/replay.rs` to use the framer. Map open errors by `io::ErrorKind` as today; map framing I/O errors to `ReplayError::Corrupt`. Keep `validate_meta`, `parse_record`, and the partial-tail warn-and-break decision.

4. Rewrite `data_dir::read_session_header` in `src/config/data_dir.rs` to use the framer on a `BufReader<File>`, preserving the version check and error messages. This makes discovery skip leading blank lines.

5. Rewrite `sessions::read_session_info` in `src/cli/sessions.rs` to read the header through the framer (so listing skips leading blank lines), then keep scanning the same `BufReader` for the first user prompt with the existing `find_first_user_prompt`.

6. Add one focused test per consumer pinning leading-empty behavior, plus the codec unit tests. Run the routed verification and report results.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake`.

Add the module and its tests:

```
touch src/config/session_jsonl.rs   # then fill with the codec + tests
# edit src/config/mod.rs to add: pub mod session_jsonl;
```

Rewrite the four consumers (Edit each site). Then run:

```
cargo test session
cargo test replay
cargo test --test main sessions   # or the session-mode tests the suite exposes
cargo clippy --all-targets
```

Full gate before opening the pull request:

```
just ci
```

## Validation and Acceptance

Behavior a human can verify:

- `cargo test session` passes (Session::load tests, plus a new leading-empty-line test).
- `cargo test replay` passes, including the existing partial-tail and corrupt-line tests, plus a new leading-empty-line test.
- Discovery (`load_latest_session`) and listing (`list_sessions`) accept a session file with blank lines before the header where they previously ignored the file.
- A session file whose final line is a partial, non-newline-terminated JSON fragment loads and replays with a warning rather than an error, exactly as before.
- A session file whose final line is invalid JSON but newline-terminated still errors exactly as before.
- No new full loads: discovery and listing read line-by-line via `BufReader`.
- `just ci` passes.

## Idempotence and Recovery

All steps are idempotent: re-running the same edit and `cargo test` commands is safe. The changes are pure refactors of read paths; session files are only read, never written. If a consumer rewrite breaks a test, revert that one edit and re-run its tests; the codec module can remain.

## Artifacts and Notes

Expected codec behavior, as unit tests:

```
Input "a\nb\n"        -> lines 1 ("a"), 2 ("b"); no partial tail.
Input "\na\n"         -> line 2 ("a"); leading empty line skipped.
Input "a\nb"          -> line 2 ("b") has partial_tail = true.
Input ""              -> Ok(None) on first next_record.
Input "a"             -> line 1 ("a") has partial_tail = true.
```

## Interfaces and Dependencies

- `crate::config::session_jsonl::SessionFramer<R: BufRead>` --- the positioned record iterator used by all four consumers. `R: BufRead` lets discovery and listing stream from a `BufReader<File>` while full load and replay do the same; no new dependencies.
- `crate::config::session_jsonl::FramedSessionLine { line_number, text,   partial_tail }` --- the framed line yielded by the iterator.
- `crate::config::session_jsonl` is declared `pub mod` in `src/config/mod.rs` so CLI modules (`replay`, `sessions`) can reach it.

## Outcomes & Retrospective

What was achieved: all four session readers now share one framing codec, `crate::config::session_jsonl::SessionFramer`, which yields positioned record lines (1-based physical line number, trimmed text, `partial_tail` flag) while skipping empty and whitespace-only lines uniformly. `Session::load`, `replay::load_records`, `data_dir::read_session_header`, and `sessions::read_session_info` each drive it and keep their own validation, normalization, error types, and malformed-tail handling. Leading-empty-record behavior is now identical across all four (the drift being fixed), and partial-tail tolerance is preserved for full load and replay. Discovery and listing still stream line-by-line; no full loads were introduced.

Verified against the issue's acceptance notes: v4 header handling preserved; replay exit records, requested/header UUID checks, and legacy timestamps unchanged; discovery/listing lightweight; leading-empty and partial-tail identical; `cargo test session`, replay, and session-mode tests pass; `just ci` passes. Completion hinged on keeping the per-function cyclomatic and CRAP coverage gates green: `read_session_info` was split to stay within its complexity budget, and replay's read-error branch is covered by a directory-open unit test.

What remains: none. #102 is deliberately out of scope.
