# Expand the session metrics dashboard to full detail with change attribution

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

The daily dashboard at `scripts/session-metrics/dashboard.py` and `scripts/session-metrics/dashboard.html` currently shows a curated 19-row summary. The terminal report at `scripts/session-metrics/report.py` already computes far more: every tool's failure taxonomy, Bash judge linkage, API status and retry detail, token and cache breakdowns, gate health, hook decisions, compensations, per-invocation time, and session shape. The main developer cannot use the page to decide whether a change improved behavior.

After this change, opening the generated `index.html` shows the full report for the recent window, with every tool and every failure category visible. It also groups results by the cake build that produced them, so a developer can see whether a tool, prompt, or gate change moved a metric. The report stays private, offline, and static, and it keeps the existing retained daily history.

The two product decisions are settled: cake records its build commit and dirty state in session metadata and telemetry initialization records, and raw session detail stays out of the aggregate page for a later separate owner-only detail page.

## Progress

- [x] (2026-11-03) Create the task and branch.
- [x] (2026-11-03) Add build identity metadata to Rust session and telemetry records.
- [x] (2026-11-03) Extend `cakelib.py` to load build identity and attribute invocations.
- [x] (2026-11-03) Build the full report payload and HTML sections.
- [x] (2026-11-03) Add static model, project, and version filtered pages.
- [x] (2026-11-03) Update `metrics.json` schema and retained-history behavior.
- [x] (2026-11-03) Update tests and documentation.
- [x] (2026-11-03) Run `cargo test`, `cargo fmt --check`, `just session-metrics-check`, `just check`, and preflight.
- [x] (2026-11-03) Complete the task and ExecPlan before the final implementing commit.

## Surprises & Discoveries

- Observation: `session_meta` already records the *working directory's* git commit, not the cake build's commit. Evidence: `src/config/data_dir.rs::git_state` runs `git rev-parse HEAD` in `session.working_dir`. Running cake inside the cake repository happens to identify the cake commit, but running it elsewhere does not.
- Observation: `telemetry_init` is written once per invocation, so it is the correct place to attribute a resumed session to the build that actually ran it. `session_meta` is written only when the session is created.
- Observation: the current dashboard's `aggregate()` rows are pinned to `summary.py` by `tests/test_summary.py`; the new full report must preserve that agreement surface.
- Observation: the local live metrics directory is unreadable from this sandbox (`Operation not permitted`), so verification uses synthetic fixtures and the checked-in report tests rather than the developer's real history.
- Observation: a 12-session synthetic fixture exercises every section and all filters in under a second, so server-rendered views remain practical for the daily refresh.
- Observation: a short commit prefix can collapse distinct test commits whose leading bytes are zeros. The attribution identity therefore keeps the full commit instead of a display prefix.

## Decision Log

- Decision: generate one static HTML page per model, project, and cake version, plus the all-data page, instead of moving aggregation into browser JavaScript. Rationale: the session-metrics suite is stdlib-only and offline, and Python-side aggregation keeps every rate, denominator, and percentile covered by unit tests. The filter nav is ordinary links, so the pages still work from `file://` with no server.
- Decision: keep the existing `dashboard.aggregate()` daily rows unchanged for history and the summary agreement test. Add a separate `build_report()` structure for the detail page and attribution. Rationale: the history files and `summary.py` agreement are compatibility surfaces; a parallel detail builder can evolve without breaking them.
- Decision: primary attribution key is the cake build commit plus dirty state; fall back to `cake_version`, then to `unknown`. Rationale: local development builds usually share a version, so version alone cannot separate commits; telemetry init is per invocation, so resumed sessions are attributed to the build that ran them.
- Decision: use an additive `build_commit: Option<String>` and `build_dirty: Option<bool>` on `session_meta`, `StreamRecord::SessionMeta`, and `telemetry_init`, omitted when unavailable. Rationale: session JSONL format version 4 stays valid; consumers already ignore unknown optional fields; no migration is needed.
- Decision: implement build metadata capture with a small `build.rs` that runs `git rev-parse HEAD` and `git status --porcelain`, writing empty values when git is unavailable. Rationale: no new dependency, no runtime git call, and a source tarball or non-git build still compiles and omits the fields.
- Decision: the aggregate page never embeds raw prompts or tool output. The report shows bounded metadata, full error strings that cake already stores in telemetry, and session/invocation identifiers. Raw records remain in the session JSONL for the later owner-only detail page.
- Decision: keep the full build commit in the displayed identity rather than a short prefix. Rationale: distinct commits can share a prefix, and the owner needs an unambiguous attribution key; the table scrolls instead of truncating.
- Decision: `metrics.json` moves to schema version 2 and keeps the retained daily `rows` plus an attribution list. Rationale: the HTML is now the detailed artifact, while the JSON remains the small machine-readable history and change-attribution surface.

## Outcomes & Retrospective

The dashboard is now the overview report. `index.html` renders provenance, headline metrics, build attribution, outcomes, tools, API, tokens, cache, gates, historical judge, time, hooks, compensations, sessions/invocations, and a data dictionary. Static `model-*.html`, `project-*.html`, and `version-*.html` views reuse the same builders, so every number is covered by the Python suite and the page works offline from `file://`.

Follow-up (2026-11-03): the uncapped full-detail default produced a 61 MB `index.html` and froze Chrome. The dashboard now caps any table above `--detail-limit` (default 200) and reports the full row count; the duplicate per-call tables were removed from the time section. The session and telemetry sources still hold every record, and `--detail-limit 0` restores the uncapped page. This supersedes the no-truncation rule for the default view.

Follow-up (2026-11-03): filter groups with more than eight values collapse behind a native `<details>` disclosure, so the 28-model and 149-project lists no longer dominate the nav. The count stays visible in the summary, and expanded links wrap; no JavaScript is involved.

Build identity is recorded additively in `session_meta` and `telemetry_init` as `build_commit` plus `build_dirty`. Existing format version 4 files remain valid, and older records fall back to `cake_version`. Transcript records are attributed to the invocation interval that contains their timestamp, with the session's creation build as the fallback; telemetry records use the invocation's own build.

The retained daily history and `dashboard.aggregate()` stayed unchanged, and the headline window is pinned to `summary.py` by tests. Per-session raw record pages and commit-filter pages remain future work; the aggregate page stays metadata-only as decided.

## Context and Orientation

Cake is a Rust 2024 binary-only AI coding assistant. Persisted session transcripts are JSONL under `~/.local/share/cake/sessions/`; telemetry sidecars are NDJSON under `~/.cache/cake/session-telemetry/`. The Python analysis suite under `scripts/session-metrics/` reads both. It is stdlib-only, offline, and tested with `python3 -m unittest discover -s scripts/session-metrics/tests`.

The current dashboard is generated by `dashboard.py`. It calls `cakelib.load(ns)`, builds daily aggregate rows with `aggregate()`, writes `history/YYYY-MM-DD.json`, writes `metrics.json` with `schema_version: 1`, and substitutes the JSON into `dashboard.html`'s `__METRICS__` placeholder. The HTML's inline JavaScript computes the summary table and model/project filters. The page is opened from disk and never makes a network request.

`report.py` imports eleven section modules in `SECTIONS`: `overview`, `tokens`, `cache_breaks`, `tools`, `api`, `gates`, `judge`, `time_breakdown`, `outcomes`, `hooks`, and `compensations`. Several expose reusable aggregation helpers: `tools.bash_reason_coverage`, `tools.bash_judge_outcomes`, `api.aggregate`, `gates.aggregate`, `judge.aggregate`, `cache_breaks.scan`, `time_breakdown.invocation_timeline`, `outcomes.denial_counts`, and `compensations.count_events`.

`cakelib` owns the shared data model: `Session`, `Invocation`, `Dataset`, loading, window attribution, tool-call pairing, failure classification, gate outcome classification, judge outcome classification, usage totals, and formatting. `Session` currently parses `cake_version`, `model`, `working_directory`, and `format_version` from `session_meta`. `Invocation` parses `telemetry_init` into `init` and borrows the transcript's working directory when the telemetry record does not carry one.

The Rust authority for session records is `src/types/session.rs`; the authority for telemetry records is `src/session_telemetry.rs`. Session metadata is constructed in `src/config/data_dir.rs::session_meta_record`, and telemetry initialization is constructed in `src/main.rs::attach_session_telemetry`. Session records are documented in `docs/integrations.md` and `scripts/session-metrics/README.md`.

## Plan of Work

### Milestone 1: Build identity metadata

Add `src/build_info.rs` with `commit()` and `dirty()` returning `Option<String>` and `Option<bool>` from compile-time environment variables set by a new `build.rs`. The build script runs git only when both commands succeed; otherwise it sets empty values so the fields are omitted. Add `mod build_info;` to `src/main.rs`.

Add optional `build_commit` and `build_dirty` fields to `SessionRecord::SessionMeta`, `StreamRecord::SessionMeta`, and `SessionTelemetryRecord::TelemetryInit`. Serialize them only when present. Populate session metadata in `src/config/data_dir.rs` and telemetry initialization in `src/main.rs`. Update the stream conversion, every Rust constructor, and the focused serialization tests. Format version 4 does not change.

Acceptance: `cargo test` passes; a fresh session file and telemetry sidecar contain the current commit and dirty state when the build ran from a git checkout, and omit both fields when the values are unavailable. The focused serde tests pin presence, absence, and the additive shape.

### Milestone 2: Reader support and attribution data

Extend `cakelib.Session` with `build_commit` and `build_dirty`. Extend `Invocation` with transcript fallbacks for `cake_version`, `build_commit`, and `build_dirty`, and expose `build_commit`, `build_dirty`, and `build_identity` properties. Thread a session-metadata map through `load_telemetry` so an invocation can borrow its session's identity when its init record omits it. Keep the existing `working_directory` fallback behavior.

Add a `dashboard.change_attribution(data, start, end)` builder that groups sessions, tasks, tool calls, API attempts, retries, gate decisions, token usage, and durations by `build_identity`. Include the first and last day, model/project mix, and the latest-versus-previous comparison.

Acceptance: unit tests construct an old session and a resumed invocation with different build identities and prove the invocation is attributed to the build recorded on its init record, not the session's creation build. A session without telemetry falls back to its session metadata identity.

### Milestone 3: Full report sections

Add a `dashboard.build_report(data, start, end, now)` builder returning an ordered list of sections. Each section is a dictionary with an id, title, optional note, optional cards, and tables. Each table has headers, rows, and numeric-column metadata; each rate cell is formatted as `percentage (numerator/denominator)`; each percentile table carries its sample count. No builder truncates rows.

The sections are:

- Provenance and coverage.
- Headline metrics for the last seven complete days versus the preceding seven, using `summary.summarize` for the shared vocabulary and dashboard-specific counters for tool, cache, token, and duration detail.
- Change attribution, including the per-identity table, latest-versus-previous comparison, model and project mix, and a daily timeline with identity.
- Outcomes, including every subtype, task shape, denials, tasks per session, invocation summaries, all failure reasons, turns per session, and every abnormal termination.
- Tools, including every tool, the full failure taxonomy, Bash reason coverage, the full Bash judge outcome matrix, judge linkage accounting, retry recovery, per-turn parallelism, per-tool telemetry durations and output sizes, tool failures by model, project, and build identity, and every tool call sorted by duration.
- API, including all status codes, all error variants, all termination classes, latency by model, all retry reasons, context-overflow overrides, attempts per invocation, failures by model/project/identity, and every attempt sorted by duration.
- Tokens, including totals, by model, project, day, and identity, per invocation, context growth, transcript cross-check, and tokens per task.
- Cache breaks, including causes and every miss detail.
- Gates, including every gate, every startup and decision outcome, failure classes, per model, per identity, and evidence counts. Gate evidence data is counted, never printed.
- Historical judge, including every terminal class, phase latency, token usage, retries, deadline ratios, and status codes.
- Time, including every invocation timeline, aggregate work, tool work, API time by model, hook time, turn pacing, think time, and all slowest calls and attempts.
- Hooks and skills, including every event, decision, denial, infrastructure error, exit code, command, duration, and skill activation.
- Compensations, including every model and kind, every detail, judge latency, and allowlist overrides.
- Sessions and invocations, including every session and every invocation with model, project, identity, counts, tokens, and outcome.
- Data dictionary, defining every metric, numerator, denominator, source, and caveat.

Acceptance: a synthetic dataset that exercises every section produces every table and every row; a test asserts that a sentinel tool, error variant, status code, gate name, hook command, and session id are all present, proving no top-N truncation. An empty dataset still produces every section with an explicit "no data" row.

### Milestone 4: Static HTML and filtered pages

Replace the inline-JavaScript aggregation in `dashboard.html` with a server- rendered template. The template receives a navigation block, a provenance block, and a body block. Add CSS for dense tables, sticky headers, scroll containers, cards, badges, and monospace identifiers. Every table is rendered in full inside a scroll container; no client-side truncation or sorting is required.

`dashboard.generate()` writes `index.html`, one `model-*.html` per model, one `project-*.html` per project, and one `version-*.html` per cake version. Each filtered page is produced by filtering the loaded `Dataset` and calling the same `build_report()` and renderer, so every number on every page is computed in Python. Filter names are sanitized slugs derived from the value plus a short hash; the visible label is the original value.

Keep the history behavior: recompute the recent 31 UTC dates, retain older history files, and write `metrics.json` with `schema_version: 2`, the retained `rows`, the attribution rows, and the measured/window provenance. Preserve the 0700 output directory, 0700 history directory, and `<`/`>`/`&` escaping in the HTML.

Acceptance: `generate()` produces the main page and filter pages, every page loads without a network request, the model/project/version links navigate to the matching page, and a malicious model/project string cannot break the HTML or inject a script. History retention and owner-only modes remain.

### Milestone 5: Documentation and verification

Update `scripts/session-metrics/README.md` for the new page sections, filter pages, `metrics.json` schema 2, and build identity attribution. Update `docs/integrations.md` for the additive `build_commit`/`build_dirty` fields on `session_meta`, stream-json `session_meta`, and `telemetry_init`, including absence semantics.

Run `cargo fmt --check`, `cargo test`, `just session-metrics-check`, and the change-risk/documentation checks required by `CONTRIBUTING.md`. Run preflight. Complete the task's Acceptance Notes, fill this plan's Outcomes & Retrospective, move the plan to `docs/exec-plans/completed/`, and complete the ahm task before the final implementing commit.

## Concrete Steps

From `/Users/travisennis/Projects/cake/cake`:

```
cargo test

python3 -m unittest discover -s scripts/session-metrics/tests -v

python3 scripts/session-metrics/dashboard.py --output /tmp/cake-dashboard
open /tmp/cake-dashboard/index.html
```

Inspect `/tmp/cake-dashboard/index.html` for every section, and open `/tmp/cake-dashboard/model-*.html` to confirm the filter view changes the numbers. Inspect `/tmp/cake-dashboard/metrics.json` for `"schema_version": 2` and the attribution rows.

To verify build identity recording without the live metrics directory, run a single fake-provider CLI test or inspect a newly created session and sidecar from a test harness: the first `session_meta` line and the `telemetry_init` line must contain `build_commit` and `build_dirty` when the build ran from a git checkout.

## Validation and Acceptance

The page shows full detail when opened from disk: all tools, all failure categories, all API statuses and errors, all gate outcomes, all hook decisions, all session and invocation rows, and all change-attribution rows. Rate cells read like `2.4% (6/250)`, and percentile tables show their sample count. A synthetic dataset with more than ten tools, errors, statuses, and sessions shows every one of them; nothing is cut to a top-N list. Model, project, and version links open filtered pages whose headline counts match the filter.

`cargo test` passes with the additive build metadata tests. The Python suite passes with tests for section coverage, denominator formatting, no truncation, filter pages, escaping, empty data, history retention, and old-history tolerance.

## Idempotence and Recovery

`dashboard.generate()` replaces only the current 31 daily history files and rewrites the HTML/JSON outputs atomically. Older history files are retained. Re-running the generator is safe. If a filter slug collides, the short hash keeps the filename distinct. If the session directory is missing, the generator exits without replacing the previous page.

## Artifacts and Notes

Generated views from the synthetic 12-session fixture: `index.html`, `model-model-0-*.html`, `model-model-1-*.html`, `model-model-2-*.html`, `project-projects-project-{0..3}-*.html`, and `version-1.0.{0,1}-*.html`. The main page contains these sections in order: provenance, headline, attribution, outcomes, tools, API, tokens, cache, gates, judge, time, hooks, compensations, sessions, definitions. The fixture exercises all 12 tool names, all 12 API statuses (500-511), all session ids, all four gate names, all 12 hook commands, and all four skill names; a test asserts each sentinel is present, proving no top-N truncation.

Validation run on 2026-11-03:

- `cargo test` passed (1831 unit tests plus all integration tests).
- `cargo fmt --check` passed.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `just check` passed (toolchain, formatting, complexity, strict clippy, all tests, import/dependency/module-size/domain-glossary lints, and script fixtures).
- `just session-metrics-check` passed with 173 tests.
- `python3 -m py_compile scripts/session-metrics/*.py` passed.
- `python3 scripts/session-metrics/dashboard.py --output /tmp/cake-dashboard` is the manual smoke command; the sandbox could not read the live metrics directory, so verification used the synthetic fixture and the test suite.

## Interfaces and Dependencies

New Rust surface:

- `crate::build_info::commit() -> Option<String>` and `crate::build_info::dirty() -> Option<bool>`.
- `SessionRecord::SessionMeta` and `StreamRecord::SessionMeta` gain optional `build_commit` and `build_dirty` fields.
- `SessionTelemetryRecord::TelemetryInit` gains optional `build_commit` and `build_dirty` fields.

New Python surface:

- `cakelib.Session.build_commit`, `cakelib.Session.build_dirty`, `cakelib.Session.build_identity`.
- `cakelib.Invocation.build_commit`, `cakelib.Invocation.build_dirty`, `cakelib.Invocation.build_identity`.
- `dashboard_report.build_report(data, start, end, now)` returning the ordered section dictionaries.
- `dashboard_report.change_attribution(data, start, end)` returning attribution rows.
- `dashboard_render.render_page(title, subtitle, nav, sections, footer)` returning one static HTML page.

No new dependencies. The Python suite remains stdlib-only and offline. The HTML remains a single local file per view with no external assets and no network requests.
