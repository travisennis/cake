## Show where one Cake invocation spends its wall time

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective stay current as work proceeds.

## Purpose / Big Picture

`just session-metrics` currently reports cumulative API and tool durations, but a person investigating a slow task cannot see which activity occupied its wall time. Parallel tool durations overlap, and scheduled retry delays are not observed waits. After this change, the report will show a selected invocation's wall-time timeline with exclusive activity buckets, a named residual, and the slowest operations. Existing session transcripts and older telemetry remain readable.

## Progress

- [x] (2026-09-22) Inspect telemetry producers, report loader, and existing timing tests; create issue #637 and branch.
- [x] (2026-09-22) Add bounded timing metadata for tool spans and observed main-provider retry waits.
- [x] (2026-09-22) Add per-invocation timeline attribution and selection to the existing metrics report.
- [x] (2026-09-22) Test concurrent tools, old records, retries, and real-session rendering; update telemetry and report docs.
- [x] (2026-09-22) Run preflight and routed checks, complete the issue record, and prepare the pull request.

## Surprises & Discoveries

- The existing `time_breakdown.py` correctly refuses an exclusive remainder because `tool_call` has only duration, and `retry_scheduled` records only intended delay.
- The reporter already groups telemetry by `invocation_id`; the new display can build on that rather than introduce a separate metrics command.
- Existing real sidecars predate tool spans; the report assigns their provider attempts and names the tool coverage gap rather than treating missing spans as zero.

## Decision Log

- Decision: Keep timing changes in additive telemetry fields and records. Do not change session JSONL or completion output. Date: 2026-09-22.
- Decision: Attribute parallel operations by interval union and label unobserved time as residual. Parent `tb__subagent` waits remain a separate category, not summed with child sessions. Date: 2026-09-22.

## Outcomes & Retrospective

The report now selects a session or invocation by exact UUID, attributes observed intervals without double-counting parallel calls, and prints a residual and missing-span counts. Tool spans and retry waits are additive telemetry metadata. Focused telemetry integration tests, all 126 session-metrics tests, `just docs-check`, and the full `just check` gate passed. Historical tool spans remain unavailable by design.

## Context and Orientation

`src/clients/agent/agent_loop.rs` measures tool durations, and `src/session_telemetry.rs` defines sidecar records. `src/clients/agent_runner.rs` awaits main-provider retry delays. `scripts/session-metrics/cakelib.py` loads sidecars by invocation, and `scripts/session-metrics/time_breakdown.py` prints aggregate time. The sidecar is diagnostic metadata; it does not drive resume or model history. A tool interval is elapsed wall time from starting a tool through its result; concurrent intervals overlap. An exclusive bucket gives any instant of invocation wall time to at most one activity.

## Plan of Work

Add optional start/end timestamps to `ToolCallTelemetry` when `Agent::run_tool_call` executes, including immediate hook-blocked results. Add an observed retry-wait record after each main-provider backoff in `agent_runner.rs`, retaining the existing scheduled record for in-flight diagnosis. In `time_breakdown.py`, select an invocation by session/invocation id, construct bounded intervals for provider attempts, tool calls, and observed retry waits, clip them to the invocation, and sweep interval boundaries to assign exclusive time. Treat a subagent tool call as delegated wait. Show Bash safety decision duration as an observed subset of Bash tool time using existing judge compensation linkage, and label the remaining Bash time without claiming it is only child-process execution. Preserve the aggregate report for historical sidecars and explain missing timing fields. Add focused synthetic tests and documentation in `docs/integrations.md` and `scripts/session-metrics/README.md`.

## Concrete Steps

From `/Users/travisennis/Projects/cake/cake-0`, run `just session-metrics-check` after Python changes and `cargo test session_telemetry` after telemetry changes. Run `just check` after the final source edit. Run `python3 scripts/session-metrics/time_breakdown.py --days 1 --session <id>` against a recent sidecar written by the current build or a synthetic fixture; expect a selected invocation row with category durations totaling its wall duration plus an explicit residual. Then run preflight and `git diff --check`, commit the specific changed paths, push, and open a labeled PR closing #637.

## Validation and Acceptance

One selected invocation shows its session and invocation IDs, duration, exclusive main-provider/tool/retry/residual buckets, separate delegated wait and Bash safety information, and slowest operations. Parallel tools do not inflate wall time. Historical records without tool intervals or observed waits print unknown coverage and retain aggregate measurements. The report does not reveal commands or prompts. Existing Cake CLI and session record shapes are unchanged.

## Idempotence and Recovery

The report is read-only. Tests can be rerun. Telemetry additions are optional, so an interrupted task or an older sidecar stays readable. If network work fails after local validation, retry issue/PR operations without recreating the branch or changes.

## Artifacts and Notes

Issue: https://github.com/travisennis/cake/issues/637. On a historical 265-second invocation, the selected report showed 3.3 minutes in the main provider, 1.1 minutes unobserved, and 57 older tool calls explicitly missing spans. The timeline tests assign a synthetic 1,000 ms invocation to 300 ms provider, 100 ms retry, 80 ms Bash safety estimate, 220 ms Bash remaining, 100 ms other tools, 150 ms parallel activity, and 50 ms unobserved.

### How did we do?

The change is within one telemetry/reporting concern. It preserves Cake's session JSONL and model-visible behavior while making new sidecars useful for task timing.

### Feedback to keep

Keep the explicit `parallel activity`, `unobserved`, and missing-span labels. They prevent cumulative work or historical omissions from reading as exclusive wall time.

### Feedback to ignore

An exact Bash child-process span would require additional instrumentation at the tool boundary; the present report labels its safety portion as an estimate and names what `Bash remaining` contains. That extension is outside this issue's acceptance scope.

### Plan of attack

No code fixes remain after the Clippy line-limit and Markdown alignment corrections. Commit the tested diff and open the PR.

### Preflight compliance

Root `AGENTS.md`, issue #637, this ExecPlan, `ARCHITECTURE.md`, `docs/integrations.md`, `CONTRIBUTING.md`, and the changed Rust/Python code and tests were read. There are no nested AGENTS.md files. Three passes covered rules and docs, correctness/source of truth, and simplification. `cargo test --test session_telemetry --quiet`, `just session-metrics-check`, `just docs-check`, `just check`, `cargo fmt --check`, and `git diff --check` passed. An initial sandboxed telemetry test run could not bind a local mock-server port; the rerun with the required access passed.

## Interfaces and Dependencies

`ToolCallTelemetry` adds optional UTC span fields, and a new `SessionTelemetryRecord` variant records observed retry wait. `time_breakdown.py` consumes these fields through the existing `Invocation` loader. No new dependency is required.
