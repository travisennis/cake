# Session Metrics

Stdlib-only Python scripts that report how well cake is working, from the two places cake records evidence:

- **Session transcripts** --- `~/.local/share/cake/sessions/{uuid}.jsonl` (or `{CAKE_DATA_DIR}/sessions`). Conversation records: `session_meta`, `message`, `function_call`/`function_call_output`, `reasoning`, `hook_event`, `task_start`/`task_complete`, `skill_activated`, plus legacy `session_start`/`init`/`result` from older format versions.
- **Telemetry sidecars** --- `~/.cache/cake/session-telemetry/{uuid}.ndjson`. Operational records: `telemetry_init` (run mode, api type, settings), `api_attempt` (conversation-model latency, status, and token usage per request), `judge_attempt` (command-safety judge model controls, phase timing, status, termination, and token usage without raw prompts), `retry_scheduled`, `retry_wait` (observed backoff span), `tool_call` (duration, span, output bytes, error flag), `compensation` (bounded kind and detail; judge events also carry the one-way digest that links the event to its transcript tool call), `session_summary` (success, duration, turns, total usage). `api_attempt.usage.input_tokens_details` may include provider-reported `cached_tokens` and `cache_write_tokens`; the metrics suite uses these optional fields for after-the-fact cache-break analysis.

## Usage

```bash
just session-metrics                  # everything, last 30 days
just session-metrics --days 0        # all time
python3 overview.py --days 7          # one section
python3 tools.py --model deepseek     # filter by model substring
python3 tokens.py --project cake      # filter by working-directory substring
python3 time_breakdown.py --session UUID  # one session's invocations
python3 time_breakdown.py --session UUID --invocation UUID  # one task run
```

Every script accepts `--days`, `--sessions-dir`, `--telemetry-dir`, `--model`, `--project`, `--session`, and `--invocation`. The ID filters require full, exact UUIDs. `report.py` loads the data once and runs all sections against it.

## Scripts

  | Script              | Reports                                                                                                                                                                                                                                                       |
  | ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | `overview.py`       | Sessions per day, models, cake/format versions, run modes (new/continue/resume/fork), api types, top projects, per-session shape                                                                                                                              |
  | `tokens.py`         | Token totals with cache-read and cache-write counts, cache-hit rates, by model/project/day, per-invocation distribution, context growth per request                                                                                                           |
  | `cache_breaks.py`   | After-the-fact prompt-cache break detection from telemetry, with missed-token totals and likely model-switch, idle-TTL, or generic causes                                                                                                                     |
  | `tools.py`          | Per-tool call volume and failure rates, failure taxonomy, Edit/Write retry recovery, durations/output sizes, per-turn parallelism, Bash `reason` coverage per model, and Bash judge outcomes (allow/warn/block/fail-closed/bypass) split by `reason` presence |
  | `api.py`            | API attempt failures, status codes, latency percentiles by model, retry reasons/delays, context-overflow overrides                                                                                                                                            |
  | `judge.py`          | LLM-judge reliability from `judge_attempt` telemetry: latency and phase timing, terminal classes (timeout/transport/...) and rates, retries, token cost, status codes, near-timeout attempts                                                                  |
  | `time_breakdown.py` | Per-invocation exclusive wall-time timeline when spans exist, plus API elapsed time, cumulative tool work, scheduled retry delays, turn pacing, think time between tasks, and slowest operations                                                              |
  | `outcomes.py`       | Task and invocation success rates, error subtypes, duration/turn distributions, per-session turn totals, permission denials, abnormal terminations                                                                                                            |
  | `hooks.py`          | Hook events, allow/deny decisions, denied commands, hook errors and durations, skill activations                                                                                                                                                              |
  | `compensations.py`  | Model-compensation counters per model: json repair, judge verdicts/fail-closed/bypass, same-path serialization, output truncation, Edit invalid args, context-overflow retries                                                                                |
  | `cakelib.py`        | Shared loading, call pairing, error classification, formatting (not a CLI)                                                                                                                                                                                    |

## Compensation review

Each counter in `compensations.py` maps to a compensation cake carries for a model weakness (the Bitter Lesson's operational corollary: hand-coded knowledge needs an expiration review). A counter flatlined at zero for a given model means the model no longer needs that compensation, which makes the compensation a **deletion candidate**: open a review, and delete the compensation (or rework the prompt) only with measurement and a test that proves the behavior is still protected. Judge verdict, fail-closed, and bypass counters are recorded by the LLM-judge preflight (issue #72 Milestone 5): every Bash call emits one judge event (verdict + code + latency, or the failure class, or a bypass).

## Bash `reason` and judge outcomes

`tools.py` owns the Bash `reason` coverage table and the judge-outcome table that correlates it, and it is the only report that joins transcript calls to judge telemetry.

The pairing rule is the **SHA-256 hex digest of the transcript's raw `call_id`**: the `function_call`/`function_call_output` pair carries the raw identifier, and the judge event carries `call_id`, the digest `digest_identifier` in `src/session_telemetry.rs` produces from it (the same function behind `judge_attempt.call_id`). The join is restricted to one session (`session_id`) and never uses timestamps or record order, so concurrent and out-of-order records pair identically. The session is as narrow as the join can go: a transcript records no invocation id, so a `(session, invocation)` scope would have to pair by timestamp or record position, which the join refuses to do.

One call and one judge event per digest is the only pair the report counts. A digest that matches more than one call or more than one judge event is reported as `ambiguous` instead of being guessed at. A judge retry is counted once per call --- the report counts the terminal judge event, and the judge-attempt records only feed the retried-call note --- so a call that failed over contributes one outcome.

The outcome columns are the verdicts (`allow`, `warn`, `block`), the judge outcomes without a verdict (`fail-closed`, `bypass`), and the join's own bookkeeping: `no call id` (the call has no provider-assigned identifier, so there is nothing to join on), `no sidecar` (the session has no telemetry records in the window), `no event` (the session has telemetry, but no judge event carries this call's digest), `ambiguous`, and `other` (a kind or verdict value outside the vocabulary, kept visible rather than dropped). The three `no *` columns are one call's worth of missing outcome each, split by cause so the table says *why* a group has nothing to correlate. The event side is accounted the same way, separately: `no linkage key` (the event carries no digest at all) and `call outside the window` (its transcript call is not in the window). Both sides are printed, so nothing is silently discarded, and `fail_closed`, `bypass`, `no call id`, `no sidecar`, `no event`, `ambiguous`, and `other` are
not verdicts, which is why the block and warn rates divide by `allow + warn + block` and never by the call count.

Judge events recorded before cake carried that digest have no event-side key, so on a corpus recorded by an older build every judge event is reported as `no linkage key`, every call as `no event`, and the correlation stays empty until runs are recorded on a build that has it. The judge-attempt digest is older than the event field, so the retried-call accounting is populated either way.

The table is metadata-only: it reports the presence of `reason`, the bounded outcome vocabulary, the model, and aggregate counts. The command text, the reason text, the raw call id, and the digest itself never enter the report.

Run the metrics tests with `just session-metrics-check`.

## Reproducibility

Numbers move while a session is running, because the report reads the transcript directory as it is: the session that is currently writing is included in its own measurement, and its tool calls, tokens, and task counts keep changing until it ends. Every section therefore carries the same provenance line, printed by `cakelib.describe_window`:

```text
Window: last 1 days | sessions: 12 (/Users/.../sessions) | telemetry invocations: 27 (/Users/.../session-telemetry) | measured 2026-09-17T20:03:00Z | newest activity 2026-09-17T20:02:56Z (4s ago) | unfinished tasks in 2 session(s)
```

`measured` is when this run read the data, `newest activity` is the newest record it saw, and the `unfinished tasks` count is the sessions whose final task started but never completed (live, crashed, or abandoned). Quote a number with that stamp, or rerun and quote the new one; a number without it cannot be compared with another report.

To measure a frozen snapshot instead of the live directory, copy the transcripts and sidecars and point the report at the copies:

```bash
cp -R ~/.local/share/cake/sessions /tmp/cake-sessions-snapshot
just session-metrics --sessions-dir /tmp/cake-sessions-snapshot --days 0
```

## Caveats

- Windowing attributes records by their own timestamps: tasks by their `task_start` timestamp (joined to `task_complete` by `task_id`, since `task_complete` carries none), tool calls by their `function_call` timestamp. The file mtime is only the fallback for untimestamped/legacy records. A long-running session's old tasks stay in their own window instead of counting in the window of the session's last activity.
- Sessions whose final task started but never completed (live, crashed, or abandoned) are reported separately in `outcomes.py`; the incomplete task is excluded from the completed-task counts, and is only reported when its `task_start` is inside the window.
- Telemetry invocations are windowed by record timestamps too: an invocation counts when any of its records is in the window, so resuming a session does not pull the older invocations of the same sidecar into the report.
- Transcript tool failures are the shapes `is_tool_failure` (`cakelib.py`) matches: the `Error` prefix every tool error carries, the unprefixed `Hook blocked tool execution` prefix `PreToolUse` hook denials are stored with, and the full `[Sandbox restriction]` notice line `bash.rs` appends after a Bash filesystem denial. That notice is matched by its emitted shape, not by tool name: the line must start with the marker and with the notice's own sentence, so a file, diff, or search that quotes the marker --- including a truncated copy of the notice printed at a line start --- does not read as a failure, while any tool carrying the notice verbatim (a toolbox relay returning a subagent's denied call) does. Synthetic `not executed:` outputs written by history repair or a correction turn are deliberately not counted as failures. Telemetry `was_error` is authoritative for telemetry-covered sessions.
- Telemetry sidecars only exist for sessions run since sidecar support landed; `overview.py` prints the coverage ratio. Transcript-based sections cover all sessions.
- New sidecars give `time_breakdown.py` tool start/end timestamps and observed main-provider retry waits; it pairs the existing in-flight provider start with its completed attempt. The per-invocation timeline assigns overlapping operations to `parallel activity`, then assigns uncovered time to `unobserved`. Older records without spans retain their aggregate totals and explicitly report missing spans. The Bash safety slice is an estimate from linked judge/TypeSafe model timings; `Bash remaining` also includes setup, command execution, output handling, and hooks. Hook durations are shown separately because they can overlap tools. Incomplete invocations have no `session_summary` wall time and receive no exclusive timeline.
- Tool failure taxonomy categories are keyed to current tool error message wording (`src/clients/tools/*.rs`); filesystem denials are reported as `sandbox-blocked` under the same notice-line scoping as `is_tool_failure`, and `read-only path` matches the full write-validation message (`is read-only (added via --add-dir)` / `is in a read-only directory (added via --add-dir)`) on the first line of an Edit or Write failure, which is the shape those one-line messages are stored in, rather than the word `read-only` anywhere in the output, so a nearest-match hint, diff, or grep that quotes the message keeps its own tool's bucket. If those messages or markers change, update `classify_tool_error` and `is_tool_failure` in `cakelib.py`.
- Cache-break detection is heuristic: it requires a prior cache read or write, ignores misses at or below 1,024 tokens, and uses five minutes as the idle-TTL label threshold. Provider pricing is not stored, so the report shows missed tokens but does not estimate dollars.
