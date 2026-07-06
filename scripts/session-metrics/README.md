# Session Metrics

Stdlib-only Python scripts that report how well cake is working, from the two
places cake records evidence:

- **Session transcripts** — `~/.local/share/cake/sessions/{uuid}.jsonl`
  (or `{CAKE_DATA_DIR}/sessions`). Conversation records: `session_meta`,
  `message`, `function_call`/`function_call_output`, `reasoning`,
  `hook_event`, `task_start`/`task_complete`, `skill_activated`, plus legacy
  `session_start`/`init`/`result` from older format versions.
- **Telemetry sidecars** — `~/.cache/cake/session-telemetry/{uuid}.ndjson`.
  Operational records: `telemetry_init` (run mode, api type, settings),
  `api_attempt` (latency, status, token usage per request),
  `retry_scheduled`, `tool_call` (duration, output bytes, error flag),
  `session_summary` (success, duration, turns, total usage).

## Usage

```bash
just session-metrics                  # everything, last 30 days
just session-metrics --days 0        # all time
python3 overview.py --days 7          # one section
python3 tools.py --model deepseek     # filter by model substring
python3 tokens.py --project cake      # filter by working-directory substring
```

Every script accepts `--days`, `--sessions-dir`, `--telemetry-dir`,
`--model`, and `--project`. `report.py` loads the data once and runs all
sections against it.

## Scripts

| Script        | Reports |
| ------------- | ------- |
| `overview.py` | Sessions per day, models, cake/format versions, run modes (new/continue/resume/fork), api types, top projects, per-session shape |
| `tokens.py`   | Token totals with cache-hit rates, by model/project/day, per-invocation distribution, context growth per request |
| `tools.py`    | Per-tool call volume and failure rates, failure taxonomy, Edit/Write retry recovery, durations/output sizes, per-turn parallelism |
| `api.py`      | API attempt failures, status codes, latency percentiles by model, retry reasons/delays, context-overflow overrides |
| `time_breakdown.py` | Where wall-clock time goes (model API vs tools vs retries vs overhead), tool/model time shares, turn pacing, think time between tasks, slowest operations |
| `outcomes.py` | Task and invocation success rates, error subtypes, duration/turn distributions, permission denials, abnormal terminations |
| `hooks.py`    | Hook events, allow/deny decisions, denied commands, hook errors and durations, skill activations |
| `cakelib.py`  | Shared loading, call pairing, error classification, formatting (not a CLI) |

## Caveats

- Windowing is by file mtime, so a long-running session counts in the window
  of its last activity.
- Transcript tool failures are detected by the `Error` output prefix, matching
  how the agent loop records failed tool calls; telemetry `was_error` is
  authoritative for telemetry-covered sessions.
- Telemetry sidecars only exist for sessions run since sidecar support landed;
  `overview.py` prints the coverage ratio. Transcript-based sections cover all
  sessions.
- Tool failure taxonomy categories are keyed to current tool error message
  wording (`src/clients/tools/*.rs`); if those messages change, update
  `classify_tool_error` in `cakelib.py`.
