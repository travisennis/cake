# Session Metrics

Stdlib-only Python scripts that report how well cake is working, from the two places cake records evidence:

- **Session transcripts** --- `~/.local/share/cake/sessions/{uuid}.jsonl` (or `{CAKE_DATA_DIR}/sessions`). Conversation records: `session_meta`, `message`, `function_call`/`function_call_output`, `reasoning`, `hook_event`, `task_start`/`task_complete`, `skill_activated`, plus legacy `session_start`/`init`/`result` from older format versions.
- **Telemetry sidecars** --- `~/.cache/cake/session-telemetry/{uuid}.ndjson`. Operational records: `telemetry_init` (run mode, api type, settings), `api_attempt` (conversation-model latency, status, and token usage per request), `judge_attempt` (command-safety judge model controls, phase timing, status, termination, and token usage without raw prompts), `retry_scheduled`, `tool_call` (duration, output bytes, error flag), `session_summary` (success, duration, turns, total usage). `api_attempt.usage.input_tokens_details` may include provider-reported `cached_tokens` and `cache_write_tokens`; the metrics suite uses these optional fields for after-the-fact cache-break analysis.

## Usage

```bash
just session-metrics                  # everything, last 30 days
just session-metrics --days 0        # all time
python3 overview.py --days 7          # one section
python3 tools.py --model deepseek     # filter by model substring
python3 tokens.py --project cake      # filter by working-directory substring
```

Every script accepts `--days`, `--sessions-dir`, `--telemetry-dir`, `--model`, and `--project`. `report.py` loads the data once and runs all sections against it.

## Scripts

  | Script              | Reports                                                                                                                                                                        |
  | ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
  | `overview.py`       | Sessions per day, models, cake/format versions, run modes (new/continue/resume/fork), api types, top projects, per-session shape                                               |
  | `tokens.py`         | Token totals with cache-read and cache-write counts, cache-hit rates, by model/project/day, per-invocation distribution, context growth per request                            |
  | `cache_breaks.py`   | After-the-fact prompt-cache break detection from telemetry, with missed-token totals and likely model-switch, idle-TTL, or generic causes                                      |
  | `tools.py`          | Per-tool call volume and failure rates, failure taxonomy, Edit/Write retry recovery, durations/output sizes, per-turn parallelism                                              |
  | `api.py`            | API attempt failures, status codes, latency percentiles by model, retry reasons/delays, context-overflow overrides                                                             |
  | `time_breakdown.py` | Where wall-clock time goes (model API vs tools vs retries vs overhead), tool/model time shares, turn pacing, think time between tasks, slowest operations                      |
  | `outcomes.py`       | Task and invocation success rates, error subtypes, duration/turn distributions, per-session turn totals, permission denials, abnormal terminations                             |
  | `hooks.py`          | Hook events, allow/deny decisions, denied commands, hook errors and durations, skill activations                                                                               |
  | `compensations.py`  | Model-compensation counters per model: json repair, judge verdicts/fail-closed/bypass, same-path serialization, output truncation, Edit invalid args, context-overflow retries |
  | `cakelib.py`        | Shared loading, call pairing, error classification, formatting (not a CLI)                                                                                                     |

## Compensation review

Each counter in `compensations.py` maps to a compensation cake carries for a model weakness (the Bitter Lesson's operational corollary: hand-coded knowledge needs an expiration review). A counter flatlined at zero for a given model means the model no longer needs that compensation, which makes the compensation a **deletion candidate**: open a review, and delete the compensation (or rework the prompt) only with measurement and a test that proves the behavior is still protected. Judge verdict, fail-closed, and bypass counters are recorded by the LLM-judge preflight (issue #72 Milestone 5): every Bash call emits one judge event (verdict + code + latency, or the failure class, or a bypass).

Run the metrics tests with `just session-metrics-check`.

## Caveats

- Windowing attributes records by their own timestamps: tasks by their `task_start` timestamp (joined to `task_complete` by `task_id`, since `task_complete` carries none), tool calls by their `function_call` timestamp. The file mtime is only the fallback for untimestamped/legacy records. A long-running session's old tasks stay in their own window instead of counting in the window of the session's last activity.
- Sessions whose final task started but never completed (live, crashed, or abandoned) are reported separately in `outcomes.py`; the incomplete task is excluded from the completed-task counts, and is only reported when its `task_start` is inside the window.
- Telemetry invocations are windowed by record timestamps too: an invocation counts when any of its records is in the window, so resuming a session does not pull the older invocations of the same sidecar into the report.
- Transcript tool failures are detected by the `Error` output prefix, matching how the agent loop records failed tool calls; telemetry `was_error` is authoritative for telemetry-covered sessions.
- Telemetry sidecars only exist for sessions run since sidecar support landed; `overview.py` prints the coverage ratio. Transcript-based sections cover all sessions.
- Tool failure taxonomy categories are keyed to current tool error message wording (`src/clients/tools/*.rs`); if those messages change, update `classify_tool_error` in `cakelib.py`.
- Cache-break detection is heuristic: it requires a prior cache read or write, ignores misses at or below 1,024 tokens, and uses five minutes as the idle-TTL label threshold. Provider pricing is not stored, so the report shows missed tokens but does not estimate dollars.
