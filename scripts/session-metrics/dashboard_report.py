"""Report builders for the local session-metrics dashboard.

The terminal report remains the text authority. This module reuses its
classifiers and aggregation helpers where they are already exposed, and builds
one ordered list of sections for the HTML dashboard. Builders keep every row;
`build_report` applies the configurable detail limit as a final display cap.

All functions are pure: they read a `cakelib.Dataset` and a half-open UTC
window, and return dictionaries of display strings. The HTML renderer lives in
`dashboard_render.py`.
"""

from __future__ import annotations

from collections import Counter, defaultdict
from datetime import datetime, timedelta, timezone

import cakelib
import outcomes
import tools


DEFAULT_DETAIL_LIMIT = 200


# ---------------------------------------------------------------------------
# Display helpers
# ---------------------------------------------------------------------------

def _rate_cell(numerator: int, denominator: int) -> str:
    """A rate with its numerator and denominator, never a bare percentage."""
    if not denominator:
        return f"- (0/0)"
    return f"{100 * numerator / denominator:.1f}% ({cakelib.fmt_int(numerator)}/{cakelib.fmt_int(denominator)})"


def _rate_value(numerator: int, denominator: int) -> float | None:
    return numerator / denominator if denominator else None


def _delta_cell(current: float | None, previous: float | None, unit: str) -> str:
    if current is None or previous is None:
        return "-"
    delta = current - previous
    if unit == "rate":
        return f"{100 * delta:+.1f} pp"
    if unit == "ms":
        sign = "+" if delta > 0 else "-" if delta < 0 else ""
        return sign + cakelib.fmt_ms(abs(delta))
    return f"{delta:+,.0f}"


def _direction(current: float | None, previous: float | None, higher_is_better: bool | None) -> str:
    if current is None or previous is None or higher_is_better is None:
        return "-"
    if current == previous:
        return "unchanged"
    return "better" if (current > previous) == higher_is_better else "worse"


def _p50(values: list[float]) -> float | None:
    return cakelib.percentile(values, 50) if values else None


def _p90(values: list[float]) -> float | None:
    return cakelib.percentile(values, 90) if values else None


def _p95(values: list[float]) -> float | None:
    return cakelib.percentile(values, 95) if values else None


def _p99(values: list[float]) -> float | None:
    return cakelib.percentile(values, 99) if values else None


def _duration(value: float | None) -> str:
    return cakelib.fmt_ms(value) if value is not None else "-"


def _table(title: str, headers: list[str], rows: list[list], *,
           numeric: list[int] | None = None, note: str | None = None) -> dict:
    return {"title": title, "headers": list(headers), "rows": rows,
            "numeric": list(numeric or []), "note": note}


def _section(section_id: str, title: str, description: str = "", *,
             tables: list[dict] | None = None, cards: list[dict] | None = None,
             notes: list[str] | None = None) -> dict:
    return {"id": section_id, "title": title, "description": description,
            "tables": tables or [], "cards": cards or [], "notes": notes or []}


def _in(record: dict, fallback: datetime | None, start: datetime, end: datetime) -> bool:
    return cakelib.ts_in_window(cakelib.parse_ts(record.get("timestamp")) or fallback, start, end)


def _record_ts(record: dict, fallback: datetime | None) -> datetime | None:
    return cakelib.parse_ts(record.get("timestamp")) or fallback


def _window_invocations(data: cakelib.Dataset, start: datetime, end: datetime) -> list[cakelib.Invocation]:
    return [inv for inv in data.invocations if inv.active_between(start, end)]


def _window_attempts(data: cakelib.Dataset, start: datetime, end: datetime) -> list[tuple[cakelib.Invocation, dict]]:
    return [(inv, attempt) for inv in data.invocations for attempt in inv.attempts
            if _in(attempt, inv.mtime, start, end)]


def _window_retries(data: cakelib.Dataset, start: datetime, end: datetime) -> list[tuple[cakelib.Invocation, dict]]:
    return [(inv, retry) for inv in data.invocations for retry in inv.retries
            if _in(retry, inv.mtime, start, end)]


def _window_gate_decisions(data: cakelib.Dataset, start: datetime, end: datetime) -> list[tuple[cakelib.Invocation, dict]]:
    return [(inv, decision) for inv in data.invocations for decision in inv.gate_decisions
            if _in(decision, inv.mtime, start, end)]


def _window_gate_startups(data: cakelib.Dataset, start: datetime, end: datetime) -> list[tuple[cakelib.Invocation, dict]]:
    return [(inv, startup) for inv in data.invocations for startup in inv.gate_startups
            if _in(startup, inv.mtime, start, end)]


def _window_judge_attempts(data: cakelib.Dataset, start: datetime, end: datetime) -> list[tuple[cakelib.Invocation, dict]]:
    return [(inv, attempt) for inv in data.invocations for attempt in inv.judge_attempts
            if _in(attempt, inv.mtime, start, end)]


def _window_summaries(data: cakelib.Dataset, start: datetime, end: datetime) -> list[cakelib.Invocation]:
    return [inv for inv in data.invocations if inv.summary and _in(inv.summary, inv.mtime, start, end)]


def _window_started(data: cakelib.Dataset, start: datetime, end: datetime) -> list[cakelib.Invocation]:
    return [inv for inv in data.invocations if inv.init and _in(inv.init, inv.mtime, start, end)]


def _window_tasks(data: cakelib.Dataset, start: datetime, end: datetime) -> list[tuple[cakelib.Session, dict, datetime]]:
    rows = []
    for session in data.sessions:
        starts = session._task_start_times()
        for task in session.tasks_in_window(start, end):
            if task.get("type") == "task_complete":
                stamp = starts.get(task.get("task_id")) or session.mtime
            else:
                stamp = cakelib.parse_ts(task.get("timestamp")) or session.mtime
            rows.append((session, task, stamp))
    return rows


def _window_calls(data: cakelib.Dataset, start: datetime, end: datetime) -> list[tuple[cakelib.Session, cakelib.ToolCall]]:
    return [(session, call) for session in data.sessions
            for call in session.tool_calls_in_window(start, end)]


# ---------------------------------------------------------------------------
# Provenance and headline metrics
# ---------------------------------------------------------------------------

def _window_facts(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    tasks = _window_tasks(data, start, end)
    calls = _window_calls(data, start, end)
    attempts = _window_attempts(data, start, end)
    retries = _window_retries(data, start, end)
    gate_startups = _window_gate_startups(data, start, end)
    gate_decisions = _window_gate_decisions(data, start, end)
    judge_attempts = _window_judge_attempts(data, start, end)
    summaries = _window_summaries(data, start, end)
    started = _window_started(data, start, end)
    completed_ids = {inv.invocation_id for inv in summaries}
    abnormal = sum(inv.summary is None for inv in started)

    gate_outcomes = [cakelib.gate_decision_outcome(record) for _, record in gate_decisions]
    judge_outcomes = [cakelib.judge_event_outcome(event) for inv in data.invocations
                      for event in inv.compensations
                      if str(event.get("kind") or "").startswith("judge_")
                      and _in(event, inv.mtime, start, end)]
    judge_verdicts = [outcome for outcome in judge_outcomes
                      if outcome in cakelib.JUDGE_VERDICTS]

    task_ms = [task["duration_ms"] for _, task, _ in tasks
               if task.get("duration_ms") is not None]
    task_turns = [task.get("num_turns") or task.get("turn_count") or 0 for _, task, _ in tasks]
    task_tool_counts = [task["tool_call_count"] for _, task, _ in tasks
                        if task.get("tool_call_count") is not None]
    api_ms = [attempt["request_ms"] for _, attempt in attempts
              if attempt.get("request_ms") is not None]
    invocation_ms = [inv.summary["duration_ms"] for inv in summaries
                     if inv.summary.get("duration_ms") is not None]

    edit_calls = [call for _, call in calls if call.name == "Edit"]
    edit_failures = [call for call in edit_calls if not call.ok]
    no_match = sum(cakelib.classify_tool_error(call.name, call.output)
                   == cakelib.NO_MATCH_CATEGORY for call in edit_failures)

    cache_input = cached_input = 0
    usage_attempts = 0
    input_tokens = output_tokens = 0
    for _, attempt in attempts:
        usage = attempt.get("usage") or {}
        if usage:
            usage_attempts += 1
            input_tokens += usage.get("input_tokens") or usage.get("prompt_tokens") or 0
            output_tokens += usage.get("output_tokens") or usage.get("completion_tokens") or 0
            cached = (usage.get("input_tokens_details") or {}).get("cached_tokens")
            if cached is not None:
                cache_input += usage.get("input_tokens") or usage.get("prompt_tokens") or 0
                cached_input += cached

    compensation_events = sum(
        1 for inv in data.invocations for event in inv.compensations
        if _in(event, inv.mtime, start, end)
    )
    hook_events = [record for session in data.sessions
                   for record in session.records_in_window(start, "hook_event", end=end)]
    success = sum(task.get("subtype") == "success" for _, task, _ in tasks)
    interrupted = sum(task.get("subtype") == "interrupted" for _, task, _ in tasks)

    return {
        "sessions": sum(session.active_between(start, end) for session in data.sessions),
        "invocations": len(started),
        "invocations_completed": len(completed_ids),
        "abnormal": abnormal,
        "tasks": len(tasks),
        "success": success,
        "interrupted": interrupted,
        "task_ms": task_ms,
        "task_turns": task_turns,
        "task_tool_counts": task_tool_counts,
        "tool_calls": len(calls),
        "tool_failures": sum(not call.ok for _, call in calls),
        "edit_calls": len(edit_calls),
        "edit_failures": len(edit_failures),
        "edit_no_match": no_match,
        "api_attempts": len(attempts),
        "api_failures": sum(bool(attempt.get("error")
                                 or attempt.get("status_code") not in (200, None))
                            for _, attempt in attempts),
        "api_ms": api_ms,
        "retries": len(retries),
        "gate_startups": len(gate_startups),
        "gate_decisions": len(gate_decisions),
        "gate_failures": gate_outcomes.count("failure") + gate_outcomes.count("disabled"),
        "gate_blocks": gate_outcomes.count("block"),
        "gate_judged": sum(outcome in cakelib.GATE_DECISIONS for outcome in gate_outcomes),
        "gate_denominator": len(gate_outcomes) - gate_outcomes.count("bypass"),
        "judge_attempts": len(judge_attempts),
        "judge_failures": sum(attempt.get("terminal_class") != "verdict"
                              for _, attempt in judge_attempts),
        "judge_timeouts": sum(attempt.get("terminal_class") == "timeout"
                              for _, attempt in judge_attempts),
        "judge_blocks": judge_verdicts.count("block"),
        "judge_verdicts": len(judge_verdicts),
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "usage_attempts": usage_attempts,
        "cache_input": cache_input,
        "cached_input": cached_input,
        "invocation_ms": invocation_ms,
        "compensations": compensation_events,
        "hooks": len(hook_events),
    }


def headline_metrics(data: cakelib.Dataset, now: datetime) -> dict:
    """Seven complete UTC days versus the preceding seven, with denominators."""
    today = now.astimezone(timezone.utc).replace(hour=0, minute=0, second=0, microsecond=0)
    current_start = today - timedelta(days=7)
    previous_start = current_start - timedelta(days=7)
    current = _window_facts(data, current_start, today)
    previous = _window_facts(data, previous_start, current_start)

    specs = [
        ("Completed tasks", "count", None, lambda f: f["tasks"], lambda f: f["tasks"]),
        ("Operational completion", "rate", True, lambda f: _rate_value(f["success"], f["tasks"]),
         lambda f: (f["success"], f["tasks"])),
        ("Interrupted tasks", "count", False, lambda f: f["interrupted"], lambda f: f["interrupted"]),
        ("Abnormal terminations", "rate", False,
         lambda f: _rate_value(f["abnormal"], f["invocations"]),
         lambda f: (f["abnormal"], f["invocations"])),
        ("Sessions with activity", "count", None, lambda f: f["sessions"], lambda f: f["sessions"]),
        ("Invocations started", "count", None, lambda f: f["invocations"], lambda f: f["invocations"]),
        ("Tool calls", "count", None, lambda f: f["tool_calls"], lambda f: f["tool_calls"]),
        ("Tool failure rate", "rate", False,
         lambda f: _rate_value(f["tool_failures"], f["tool_calls"]),
         lambda f: (f["tool_failures"], f["tool_calls"])),
        ("Edit calls", "count", None, lambda f: f["edit_calls"], lambda f: f["edit_calls"]),
        ("Edit failure rate", "rate", False,
         lambda f: _rate_value(f["edit_failures"], f["edit_calls"]),
         lambda f: (f["edit_failures"], f["edit_calls"])),
        ("Edit no-match rate", "rate", False,
         lambda f: _rate_value(f["edit_no_match"], f["edit_calls"]),
         lambda f: (f["edit_no_match"], f["edit_calls"])),
        ("API attempts", "count", None, lambda f: f["api_attempts"], lambda f: f["api_attempts"]),
        ("API failure rate", "rate", False,
         lambda f: _rate_value(f["api_failures"], f["api_attempts"]),
         lambda f: (f["api_failures"], f["api_attempts"])),
        ("API latency p95", "ms", False, lambda f: _p95(f["api_ms"]), lambda f: len(f["api_ms"])),
        ("Retries scheduled", "count", False, lambda f: f["retries"], lambda f: f["retries"]),
        ("Retry rate", "rate", False,
         lambda f: _rate_value(f["retries"], f["api_attempts"]),
         lambda f: (f["retries"], f["api_attempts"])),
        ("Gate startups", "count", None, lambda f: f["gate_startups"], lambda f: f["gate_startups"]),
        ("Gate decisions", "count", None, lambda f: f["gate_decisions"], lambda f: f["gate_decisions"]),
        ("Gate failure rate", "rate", False,
         lambda f: _rate_value(f["gate_failures"], f["gate_denominator"]),
         lambda f: (f["gate_failures"], f["gate_denominator"])),
        ("Gate block rate", "rate", False,
         lambda f: _rate_value(f["gate_blocks"], f["gate_judged"]),
         lambda f: (f["gate_blocks"], f["gate_judged"])),
        ("Judge attempts", "count", None, lambda f: f["judge_attempts"], lambda f: f["judge_attempts"]),
        ("Judge timeout rate", "rate", False,
         lambda f: _rate_value(f["judge_timeouts"], f["judge_attempts"]),
         lambda f: (f["judge_timeouts"], f["judge_attempts"])),
        ("Judge failure rate", "rate", False,
         lambda f: _rate_value(f["judge_failures"], f["judge_attempts"]),
         lambda f: (f["judge_failures"], f["judge_attempts"])),
        ("Judge block rate", "rate", False,
         lambda f: _rate_value(f["judge_blocks"], f["judge_verdicts"]),
         lambda f: (f["judge_blocks"], f["judge_verdicts"])),
        ("Cache-hit rate", "rate", True,
         lambda f: _rate_value(f["cached_input"], f["cache_input"]),
         lambda f: (f["cached_input"], f["cache_input"])),
        ("Input tokens", "count", None, lambda f: f["input_tokens"], lambda f: f["usage_attempts"]),
        ("Output tokens", "count", None, lambda f: f["output_tokens"], lambda f: f["usage_attempts"]),
        ("Tokens per completed task", "count", False,
         lambda f: (f["input_tokens"] + f["output_tokens"]) / f["tasks"] if f["tasks"] else None,
         lambda f: f["tasks"]),
        ("Task duration p50", "ms", False, lambda f: _p50(f["task_ms"]), lambda f: len(f["task_ms"])),
        ("Task duration p90", "ms", False, lambda f: _p90(f["task_ms"]), lambda f: len(f["task_ms"])),
        ("Invocation duration p50", "ms", False,
         lambda f: _p50(f["invocation_ms"]), lambda f: len(f["invocation_ms"])),
        ("Compensation events", "count", False, lambda f: f["compensations"], lambda f: f["compensations"]),
        ("Hook events", "count", None, lambda f: f["hooks"], lambda f: f["hooks"]),
    ]

    rows = []
    for label, unit, better, value_fn, sample_fn in specs:
        cur = value_fn(current)
        prev = value_fn(previous)
        sample = sample_fn(current)
        sample_prev = sample_fn(previous)
        if unit == "rate":
            cur_display = _rate_cell(sample[0], sample[1])
            prev_display = _rate_cell(sample_prev[0], sample_prev[1])
        elif unit == "ms":
            cur_display = _duration(cur)
            prev_display = _duration(prev)
        else:
            cur_display = cakelib.fmt_int(cur) if cur is not None else "-"
            prev_display = cakelib.fmt_int(prev) if prev is not None else "-"
        rows.append([
            label, cur_display, prev_display,
            _delta_cell(cur, prev, unit),
            cakelib.fmt_int(sample) if isinstance(sample, int) else "-",
            _direction(cur, prev, better),
        ])

    return {
        "window": {"start": current_start.isoformat(), "end": today.isoformat(),
                   "previous_start": previous_start.isoformat(),
                   "previous_end": current_start.isoformat()},
        "rows": rows,
        "table": _table(
            "Last 7 complete UTC days vs the preceding 7",
            ["metric", "last 7 complete days", "previous 7 days", "change", "sample", "direction"],
            rows, numeric=[1, 2, 3, 4],
            note="Rates show the numerator and denominator; percentiles and token totals show their sample count. No metric is suppressed for a small sample.",
        ),
    }


def provenance_section(data: cakelib.Dataset, start: datetime, end: datetime,
                       now: datetime) -> dict:
    with_telemetry = {inv.session_id for inv in data.invocations}
    covered = sum(1 for session in data.sessions if session.id in with_telemetry)
    unfinished = sum(1 for session in data.sessions if session.inflight)
    newest = max((session.last_activity for session in data.sessions), default=None)
    rows = [
        ["detail window", f"{cakelib.fmt_utc(start)} to {cakelib.fmt_utc(end)} (half-open UTC)"],
        ["measured", cakelib.fmt_utc(now)],
        ["newest transcript activity", cakelib.fmt_utc(newest) if newest else "none"],
        ["sessions in window", cakelib.fmt_int(len(data.sessions))],
        ["sessions with telemetry", _rate_cell(covered, len(data.sessions))],
        ["telemetry invocations", cakelib.fmt_int(len(data.invocations))],
        ["unfinished final tasks", cakelib.fmt_int(unfinished)],
        ["session parse errors", cakelib.fmt_int(data.session_parse_errors)],
        ["telemetry parse errors", cakelib.fmt_int(data.telemetry_parse_errors)],
        ["telemetry directory", "present" if data.telemetry_dir.is_dir() else "missing"],
        ["sessions directory", str(data.sessions_dir)],
        ["telemetry directory path", str(data.telemetry_dir)],
    ]
    return _section(
        "provenance", "Provenance and coverage",
        "The exact source window and the data that could not be read. A missing sidecar does not hide transcript metrics.",
        tables=[_table("Window and coverage", ["field", "value"], rows)],
        notes=[
            "Completion means cake ended successfully; it does not measure correctness of the work.",
            "A session can appear without telemetry. Its transcript metrics remain visible, but per-invocation API, gate, and duration metrics are unavailable.",
        ],
    )


# ---------------------------------------------------------------------------
# Build identity and attribution
# ---------------------------------------------------------------------------

def _new_stats() -> dict:
    return {
        "sessions": set(), "invocations": 0, "started": 0, "summaries": 0,
        "tasks": 0, "success": 0, "interrupted": 0, "incomplete": 0,
        "task_ms": [], "task_turns": [], "task_tool_counts": [],
        "tool_calls": 0, "tool_failures": 0,
        "edit_calls": 0, "edit_failures": 0, "edit_no_match": 0,
        "api_attempts": 0, "api_failures": 0, "api_ms": [],
        "retries": 0, "gate_decisions": 0, "gate_failures": 0, "gate_blocks": 0,
        "gate_judged": 0, "gate_denominator": 0, "judge_attempts": 0, "judge_failures": 0,
        "input_tokens": 0, "output_tokens": 0, "cache_input": 0, "cached_input": 0,
        "compensations": 0, "hooks": 0,
        "first_day": None, "last_day": None,
        "models": Counter(), "projects": Counter(),
    }


def _touch(stats: dict, moment: datetime | None) -> None:
    if moment is None:
        return
    day = moment.date().isoformat()
    if stats["first_day"] is None or day < stats["first_day"]:
        stats["first_day"] = day
    if stats["last_day"] is None or day > stats["last_day"]:
        stats["last_day"] = day


def _add_task(stats: dict, session: cakelib.Session, task: dict, moment: datetime) -> None:
    stats["sessions"].add(session.id)
    stats["tasks"] += 1
    stats["success"] += task.get("subtype") == "success"
    stats["interrupted"] += task.get("subtype") == "interrupted"
    if task.get("duration_ms") is not None:
        stats["task_ms"].append(task["duration_ms"])
    stats["task_turns"].append(task.get("num_turns") or task.get("turn_count") or 0)
    if task.get("tool_call_count") is not None:
        stats["task_tool_counts"].append(task["tool_call_count"])
    stats["models"][session.model] += 1
    stats["projects"][session.working_directory] += 1
    _touch(stats, moment)


def _add_call(stats: dict, session: cakelib.Session, call: cakelib.ToolCall,
              moment: datetime) -> None:
    stats["sessions"].add(session.id)
    stats["tool_calls"] += 1
    stats["tool_failures"] += not call.ok
    if call.name == "Edit":
        stats["edit_calls"] += 1
        stats["edit_failures"] += not call.ok
        stats["edit_no_match"] += (not call.ok and
                                   cakelib.classify_tool_error(call.name, call.output)
                                   == cakelib.NO_MATCH_CATEGORY)
    _touch(stats, moment)


def _add_attempt(stats: dict, attempt: dict, moment: datetime) -> None:
    stats["api_attempts"] += 1
    stats["api_failures"] += bool(attempt.get("error")
                                  or attempt.get("status_code") not in (200, None))
    if attempt.get("request_ms") is not None:
        stats["api_ms"].append(attempt["request_ms"])
    usage = attempt.get("usage") or {}
    stats["input_tokens"] += usage.get("input_tokens") or usage.get("prompt_tokens") or 0
    stats["output_tokens"] += usage.get("output_tokens") or usage.get("completion_tokens") or 0
    cached = (usage.get("input_tokens_details") or {}).get("cached_tokens")
    if cached is not None:
        stats["cache_input"] += usage.get("input_tokens") or usage.get("prompt_tokens") or 0
        stats["cached_input"] += cached
    _touch(stats, moment)


def _add_invocation(stats: dict, invocation: cakelib.Invocation,
                    start: datetime, end: datetime) -> None:
    stats["invocations"] += 1
    stats["sessions"].add(invocation.session_id)
    if invocation.init and _in(invocation.init, invocation.mtime, start, end):
        stats["started"] += 1
        _touch(stats, _record_ts(invocation.init, invocation.mtime))
    if invocation.summary and _in(invocation.summary, invocation.mtime, start, end):
        stats["summaries"] += 1
        _touch(stats, _record_ts(invocation.summary, invocation.mtime))
    stats["models"][invocation.model] += 1
    stats["projects"][invocation.working_directory] += 1


def _build_intervals(data: cakelib.Dataset) -> dict[str, list[tuple[datetime, datetime, str]]]:
    intervals: dict[str, list[tuple[datetime, datetime, str]]] = defaultdict(list)
    for invocation in data.invocations:
        if not invocation.init:
            continue
        begin = _record_ts(invocation.init, invocation.mtime)
        if begin is None:
            continue
        finish = _record_ts(invocation.summary or {}, invocation.last_activity or begin)
        if finish is None or finish < begin:
            finish = begin
        intervals[invocation.session_id].append((begin, finish, invocation.build_identity))
    for values in intervals.values():
        values.sort(key=lambda item: item[0])
    return intervals


def _identity_at(session: cakelib.Session, moment: datetime | None,
                 intervals: dict[str, list[tuple[datetime, datetime, str]]]) -> str:
    if moment is None:
        return session.build_identity
    matches = intervals.get(session.id, [])
    covering = [item for item in matches if item[0] <= moment <= item[1]]
    if covering:
        return max(covering, key=lambda item: item[0])[2]
    before = [item for item in matches if item[0] <= moment]
    if before:
        return max(before, key=lambda item: item[0])[2]
    return session.build_identity


def _targets(by_identity, by_model, by_project, by_day, identity, model, project,
             moment: datetime) -> list[dict]:
    day = moment.date().isoformat()
    return [
        by_identity[identity],
        by_model[(identity, model)],
        by_project[(identity, project)],
        by_day[(day, identity)],
    ]


def _collect_attribution(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    by_identity = defaultdict(_new_stats)
    by_model = defaultdict(_new_stats)
    by_project = defaultdict(_new_stats)
    by_day = defaultdict(_new_stats)
    by_tool = defaultdict(lambda: {"calls": 0, "failures": 0})
    intervals = _build_intervals(data)

    for session in data.sessions:
        starts = session._task_start_times()
        for task in session.tasks_in_window(start, end):
            moment = (starts.get(task.get("task_id")) if task.get("type") == "task_complete"
                      else cakelib.parse_ts(task.get("timestamp"))) or session.mtime
            if not cakelib.ts_in_window(moment, start, end):
                continue
            identity = _identity_at(session, moment, intervals)
            for stats in _targets(by_identity, by_model, by_project, by_day,
                                  identity, session.model, session.working_directory, moment):
                _add_task(stats, session, task, moment)
        for call in session.tool_calls_in_window(start, end):
            moment = cakelib.parse_ts(call.timestamp) or session.mtime
            if not cakelib.ts_in_window(moment, start, end):
                continue
            identity = _identity_at(session, moment, intervals)
            for stats in _targets(by_identity, by_model, by_project, by_day,
                                  identity, session.model, session.working_directory, moment):
                _add_call(stats, session, call, moment)
            tool = by_tool[(identity, call.name)]
            tool["calls"] += 1
            tool["failures"] += not call.ok
        if session.inflight_in_window(start):
            task = session.final_task_start()
            moment = _record_ts(task or {}, session.mtime)
            if moment is not None and cakelib.ts_in_window(moment, start, end):
                identity = _identity_at(session, moment, intervals)
                by_identity[identity]["incomplete"] += 1
                by_identity[identity]["sessions"].add(session.id)

    for invocation in _window_invocations(data, start, end):
        identity = invocation.build_identity
        for stats in _targets(by_identity, by_model, by_project, by_day, identity,
                              invocation.model, invocation.working_directory,
                              invocation.init and _record_ts(invocation.init, invocation.mtime)
                              or invocation.last_activity or start):
            _add_invocation(stats, invocation, start, end)
        for attempt in invocation.attempts:
            moment = _record_ts(attempt, invocation.mtime)
            if moment is None or not cakelib.ts_in_window(moment, start, end):
                continue
            for stats in _targets(by_identity, by_model, by_project, by_day, identity,
                                  invocation.model, invocation.working_directory, moment):
                _add_attempt(stats, attempt, moment)
        for retry in invocation.retries:
            moment = _record_ts(retry, invocation.mtime)
            if moment is None or not cakelib.ts_in_window(moment, start, end):
                continue
            for stats in _targets(by_identity, by_model, by_project, by_day, identity,
                                  invocation.model, invocation.working_directory, moment):
                stats["retries"] += 1
                _touch(stats, moment)
        for decision in invocation.gate_decisions:
            moment = _record_ts(decision, invocation.mtime)
            if moment is None or not cakelib.ts_in_window(moment, start, end):
                continue
            outcome = cakelib.gate_decision_outcome(decision)
            for stats in _targets(by_identity, by_model, by_project, by_day, identity,
                                  invocation.model, invocation.working_directory, moment):
                stats["gate_decisions"] += 1
                stats["gate_failures"] += outcome in ("failure", "disabled")
                stats["gate_blocks"] += outcome == "block"
                stats["gate_judged"] += outcome in cakelib.GATE_DECISIONS
                stats["gate_denominator"] += outcome != "bypass"
                _touch(stats, moment)
        for attempt in invocation.judge_attempts:
            moment = _record_ts(attempt, invocation.mtime)
            if moment is None or not cakelib.ts_in_window(moment, start, end):
                continue
            for stats in _targets(by_identity, by_model, by_project, by_day, identity,
                                  invocation.model, invocation.working_directory, moment):
                stats["judge_attempts"] += 1
                stats["judge_failures"] += attempt.get("terminal_class") != "verdict"
                _touch(stats, moment)
        for event in invocation.compensations:
            moment = _record_ts(event, invocation.mtime)
            if moment is None or not cakelib.ts_in_window(moment, start, end):
                continue
            for stats in _targets(by_identity, by_model, by_project, by_day, identity,
                                  invocation.model, invocation.working_directory, moment):
                stats["compensations"] += 1
                _touch(stats, moment)
    return {"identities": by_identity, "models": by_model, "projects": by_project,
            "days": by_day, "tools": by_tool}


def _attribution_row(identity: str, stats: dict) -> dict:
    tokens = stats["input_tokens"] + stats["output_tokens"]
    return {
        "identity": identity,
        "first_day": stats["first_day"],
        "last_day": stats["last_day"],
        "sessions": len(stats["sessions"]),
        "invocations": stats["invocations"],
        "started": stats["started"],
        "summaries": stats["summaries"],
        "tasks": stats["tasks"],
        "success": stats["success"],
        "interrupted": stats["interrupted"],
        "incomplete": stats["incomplete"],
        "task_duration_p50_ms": _p50(stats["task_ms"]),
        "task_duration_p90_ms": _p90(stats["task_ms"]),
        "tool_calls": stats["tool_calls"],
        "tool_failures": stats["tool_failures"],
        "edit_calls": stats["edit_calls"],
        "edit_failures": stats["edit_failures"],
        "edit_no_match": stats["edit_no_match"],
        "api_attempts": stats["api_attempts"],
        "api_failures": stats["api_failures"],
        "api_latency_p95_ms": _p95(stats["api_ms"]),
        "retries": stats["retries"],
        "gate_decisions": stats["gate_decisions"],
        "gate_failures": stats["gate_failures"],
        "gate_blocks": stats["gate_blocks"],
        "gate_judged": stats["gate_judged"],
        "gate_denominator": stats["gate_denominator"],
        "judge_attempts": stats["judge_attempts"],
        "judge_failures": stats["judge_failures"],
        "input_tokens": stats["input_tokens"],
        "output_tokens": stats["output_tokens"],
        "cache_input": stats["cache_input"],
        "cached_input": stats["cached_input"],
        "compensations": stats["compensations"],
        "models": dict(stats["models"]),
        "projects": dict(stats["projects"]),
        "tokens_per_task": tokens / stats["tasks"] if stats["tasks"] else None,
        "cache_hit_rate": _rate_value(stats["cached_input"], stats["cache_input"]),
        "task_success_rate": _rate_value(stats["success"], stats["tasks"]),
        "tool_failure_rate": _rate_value(stats["tool_failures"], stats["tool_calls"]),
        "edit_failure_rate": _rate_value(stats["edit_failures"], stats["edit_calls"]),
        "edit_no_match_rate": _rate_value(stats["edit_no_match"], stats["edit_calls"]),
        "api_failure_rate": _rate_value(stats["api_failures"], stats["api_attempts"]),
        "gate_failure_rate": _rate_value(stats["gate_failures"],
                                         stats["gate_denominator"]),
        "gate_block_rate": _rate_value(stats["gate_blocks"], stats["gate_judged"]),
    }


def change_attribution(data: cakelib.Dataset, start: datetime, end: datetime) -> list[dict]:
    collected = _collect_attribution(data, start, end)
    rows = [_attribution_row(identity, stats)
            for identity, stats in collected["identities"].items()]
    rows.sort(key=lambda row: (row["last_day"] or "", row["first_day"] or "", row["identity"]))
    return rows


def _attribution_table_rows(rows: list[dict]) -> list[list]:
    result = []
    for row in rows:
        result.append([
            row["identity"],
            f"{row['first_day'] or '-'} to {row['last_day'] or '-'}",
            _rate_cell(row["success"], row["tasks"]),
            _rate_cell(row["tool_failures"], row["tool_calls"]),
            _rate_cell(row["edit_no_match"], row["edit_calls"]),
            _rate_cell(row["api_failures"], row["api_attempts"]),
            _duration(row["api_latency_p95_ms"]),
            _rate_cell(row["cached_input"], row["cache_input"]),
            _rate_cell(row["gate_failures"], row["gate_denominator"]),
            _rate_cell(row["gate_blocks"], row["gate_judged"]),
            cakelib.fmt_int(row["tasks"]),
            cakelib.fmt_int(row["sessions"]),
            cakelib.fmt_int(row["invocations"]),
            cakelib.fmt_int(row["tokens_per_task"]) if row["tokens_per_task"] is not None else "-",
            _duration(row["task_duration_p50_ms"]),
            ", ".join(sorted(row["models"])) or "-",
        ])
    return result


def _attribution_delta_rows(rows: list[dict]) -> list[list]:
    if len(rows) < 2:
        return []
    latest, previous = rows[-1], rows[-2]
    metrics = [
        ("Completed tasks", "tasks", None, "count", None),
        ("Operational completion", "task_success_rate", ("success", "tasks"), "rate", True),
        ("Tool failure rate", "tool_failure_rate", ("tool_failures", "tool_calls"),
         "rate", False),
        ("Edit no-match rate", "edit_no_match_rate", ("edit_no_match", "edit_calls"),
         "rate", False),
        ("API failure rate", "api_failure_rate", ("api_failures", "api_attempts"),
         "rate", False),
        ("API latency p95", "api_latency_p95_ms", None, "ms", False),
        ("Cache-hit rate", "cache_hit_rate", ("cached_input", "cache_input"),
         "rate", True),
        ("Gate failure rate", "gate_failure_rate", ("gate_failures", "gate_denominator"),
         "rate", False),
        ("Gate block rate", "gate_block_rate", ("gate_blocks", "gate_judged"),
         "rate", False),
        ("Tokens per task", "tokens_per_task", None, "float", False),
        ("Task duration p50", "task_duration_p50_ms", None, "ms", False),
    ]
    result = []
    for label, key, rate_keys, unit, better in metrics:
        current, reference = latest.get(key), previous.get(key)
        if unit == "rate":
            cur_num = (latest.get(rate_keys[0], 0), latest.get(rate_keys[1], 0))
            prev_num = (previous.get(rate_keys[0], 0), previous.get(rate_keys[1], 0))
            cur_display = _rate_cell(*cur_num)
            prev_display = _rate_cell(*prev_num)
        elif unit == "ms":
            cur_display, prev_display = _duration(current), _duration(reference)
        elif unit == "float":
            cur_display = f"{current:,.0f}" if current is not None else "-"
            prev_display = f"{reference:,.0f}" if reference is not None else "-"
        else:
            cur_display = cakelib.fmt_int(current) if current is not None else "-"
            prev_display = cakelib.fmt_int(reference) if reference is not None else "-"
        result.append([label, cur_display, prev_display,
                       _delta_cell(current, reference, unit),
                       _direction(current, reference, better)])
    return result


def _daily_identity_rows(collected: dict) -> list[list]:
    result = []
    for (day, identity), stats in sorted(collected["days"].items()):
        result.append([
            day, identity,
            cakelib.fmt_int(len(stats["sessions"])),
            cakelib.fmt_int(stats["invocations"]),
            cakelib.fmt_int(stats["tasks"]),
            _rate_cell(stats["success"], stats["tasks"]),
            _rate_cell(stats["tool_failures"], stats["tool_calls"]),
            _rate_cell(stats["edit_no_match"], stats["edit_calls"]),
            _rate_cell(stats["api_failures"], stats["api_attempts"]),
            _rate_cell(stats["cached_input"], stats["cache_input"]),
            cakelib.fmt_int(stats["input_tokens"] + stats["output_tokens"]),
            _duration(_p50(stats["task_ms"])),
        ])
    return result


def attribution_section(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    collected = _collect_attribution(data, start, end)
    rows = [_attribution_row(identity, stats)
            for identity, stats in collected["identities"].items()]
    rows.sort(key=lambda row: (row["last_day"] or "", row["first_day"] or "", row["identity"]))
    tool_rows = []
    for (identity, tool), stats in sorted(collected["tools"].items()):
        tool_rows.append([identity, tool, cakelib.fmt_int(stats["calls"]),
                          cakelib.fmt_int(stats["failures"]),
                          _rate_cell(stats["failures"], stats["calls"])])
    model_rows = []
    for (identity, model), stats in sorted(collected["models"].items()):
        model_rows.append([
            identity, model, cakelib.fmt_int(stats["invocations"]),
            cakelib.fmt_int(stats["tasks"]),
            _rate_cell(stats["success"], stats["tasks"]),
            _rate_cell(stats["tool_failures"], stats["tool_calls"]),
            _rate_cell(stats["api_failures"], stats["api_attempts"]),
            _duration(_p95(stats["api_ms"])),
        ])
    project_rows = []
    for (identity, project), stats in sorted(collected["projects"].items()):
        project_rows.append([
            identity, project, cakelib.fmt_int(stats["invocations"]),
            cakelib.fmt_int(stats["tasks"]),
            _rate_cell(stats["success"], stats["tasks"]),
            _rate_cell(stats["tool_failures"], stats["tool_calls"]),
            _rate_cell(stats["api_failures"], stats["api_attempts"]),
            cakelib.fmt_int(stats["input_tokens"] + stats["output_tokens"]),
        ])
    return _section(
        "attribution", "Change attribution",
        "Metrics grouped by the cake build that produced them. The build commit is the primary key because local development builds usually share a package version.",
        tables=[
            _table(
                "Per build identity",
                ["identity", "days", "completion", "tool failure", "Edit no-match",
                 "API failure", "API p95", "cache hit", "gate failure", "gate block",
                 "tasks", "sessions", "invocations", "tokens/task", "task p50", "models"],
                _attribution_table_rows(rows), numeric=list(range(2, 15)),
                note="Transcript records are attributed to the invocation interval that contains their timestamp, falling back to the session's creation build; telemetry records use the invocation's own build. See the notes below.",
            ),
            _table(
                "Latest identity vs previous identity",
                ["metric", "latest", "previous", "change", "direction"],
                _attribution_delta_rows(rows), numeric=[1, 2, 3],
                note="Previous means the identity with the next-latest last activity day. Rates show their numerator and denominator.",
            ),
            _table(
                "Tool failures by build identity",
                ["identity", "tool", "calls", "failures", "failure rate"],
                tool_rows, numeric=[2, 3, 4],
            ),
            _table(
                "Model mix by build identity",
                ["identity", "model", "invocations", "tasks", "completion",
                 "tool failure", "API failure", "API p95"],
                model_rows, numeric=[2, 3, 4, 5, 6, 7],
            ),
            _table(
                "Project mix by build identity",
                ["identity", "project", "invocations", "tasks", "completion",
                 "tool failure", "API failure", "tokens"],
                project_rows, numeric=[2, 3, 4, 5, 6, 7],
            ),
            _table(
                "Daily timeline by build identity",
                ["day", "identity", "sessions", "invocations", "tasks", "completion",
                 "tool failure", "Edit no-match", "API failure", "cache hit", "tokens", "task p50"],
                _daily_identity_rows(collected), numeric=list(range(2, 12)),
            ),
        ],
        notes=[
            "Older sessions without telemetry have no build commit; they fall back to the session's cake version, and then to `unknown`.",
            "A resumed session can span builds. Each invocation's telemetry is attributed to the build that ran it; transcript records use the invocation interval containing their timestamp.",
        ],
    )


# ---------------------------------------------------------------------------
# Outcomes
# ---------------------------------------------------------------------------

def _subtype_counts(tasks: list[dict]) -> list[list]:
    counts = Counter(task.get("subtype", "?") for task in tasks)
    total = sum(counts.values())
    return [[subtype, cakelib.fmt_int(count), _rate_cell(count, total)]
            for subtype, count in counts.most_common()]


def outcomes_section(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    tasks = _window_tasks(data, start, end)
    raw_tasks = [task for _, task, _ in tasks]
    summaries = _window_summaries(data, start, end)
    started = _window_started(data, start, end)
    finished_ids = {inv.invocation_id for inv in summaries}
    unterminated = [inv for inv in started if inv.invocation_id not in finished_ids]
    inflight = [session for session in data.sessions if session.inflight_in_window(start)]

    task_rows = [[task.get("task_id", "?"), task.get("subtype", "?"),
                  _duration(task.get("duration_ms")),
                  cakelib.fmt_int(task.get("num_turns") or task.get("turn_count") or 0),
                  cakelib.fmt_int(task.get("tool_call_count") or 0),
                  task.get("limit") or "-"]
                 for _, task, _ in tasks]
    shape_values = {
        "duration": [task["duration_ms"] for task in raw_tasks
                     if task.get("duration_ms") is not None],
        "turns": [task.get("num_turns") or task.get("turn_count") or 0 for task in raw_tasks],
        "tool calls": [task["tool_call_count"] for task in raw_tasks
                       if task.get("tool_call_count") is not None],
    }
    shape_rows = [
        [label, _duration(_p50(values)) if label == "duration" else cakelib.fmt_int(_p50(values) or 0),
         _duration(_p90(values)) if label == "duration" else cakelib.fmt_int(_p90(values) or 0),
         _duration(max(values)) if label == "duration" and values else (
             cakelib.fmt_int(max(values)) if values else "-"),
         cakelib.fmt_int(len(values))]
        for label, values in shape_values.items()
    ]

    denial_events, denied_tasks, by_source, tasks_by_source = outcomes.denial_counts(raw_tasks)
    denial_rows = [[source, cakelib.fmt_int(by_source[source]),
                    cakelib.fmt_int(tasks_by_source[source]),
                    _rate_cell(by_source[source], denial_events)]
                   for source in outcomes.DENIAL_SOURCES]
    if denial_events:
        denial_rows.append(["TOTAL", cakelib.fmt_int(denial_events),
                            cakelib.fmt_int(denied_tasks), _rate_cell(denial_events, denial_events)])

    per_session = Counter(len(session.tasks_in_window(start, end)) for session in data.sessions)
    per_session_rows = [[count, cakelib.fmt_int(sessions),
                         _rate_cell(sessions, len(data.sessions))]
                        for count, sessions in sorted(per_session.items())]

    error_counts = Counter((inv.summary.get("error") or "(none)")
                           for inv in summaries if not inv.summary.get("success"))
    error_rows = [[error, cakelib.fmt_int(count)]
                  for error, count in error_counts.most_common()]
    summary_rows = [[inv.session_id, inv.invocation_id,
                     inv.init and cakelib.fmt_utc(_record_ts(inv.init, inv.mtime)) or "-",
                     inv.model, inv.working_directory, inv.build_identity,
                     "yes" if inv.summary.get("success") else "no",
                     _duration(inv.summary.get("duration_ms")),
                     cakelib.fmt_int(inv.summary.get("turn_count") or 0),
                     inv.summary.get("error") or "-"] for inv in summaries]

    session_turns = defaultdict(list)
    for inv in summaries:
        session_turns[inv.session_id].append(inv.summary.get("turn_count") or 0)
    turn_rows = [[session, cakelib.fmt_int(len(values)), cakelib.fmt_int(sum(values))]
                 for session, values in sorted(session_turns.items())]

    under_rows = [[inv.session_id, inv.invocation_id,
                   inv.init and cakelib.fmt_utc(_record_ts(inv.init, inv.mtime)) or "-",
                   inv.model, inv.working_directory, inv.build_identity,
                   cakelib.fmt_int(len(inv.attempts)), cakelib.fmt_int(len(inv.tool_calls)),
                   cakelib.fmt_int(len(inv.gate_decisions)),
                   inv.last_activity and cakelib.fmt_utc(inv.last_activity) or "-"]
                  for inv in unterminated]

    inflight_rows = []
    intervals = _build_intervals(data)
    for session in inflight:
        task = session.final_task_start() or {}
        moment = _record_ts(task, session.mtime)
        inflight_rows.append([session.id, task.get("task_id", "?"),
                              moment and cakelib.fmt_utc(moment) or "-",
                              session.model, session.working_directory,
                              _identity_at(session, moment, intervals),
                              cakelib.fmt_utc(session.last_activity)])

    return _section(
        "outcomes", "Outcomes",
        "Every completed task subtype, the shape of those tasks, permission denials, invocation outcomes, and every abnormal termination candidate.",
        tables=[
            _table("Task outcomes", ["task", "subtype", "duration", "turns", "tool calls", "limit"],
                   task_rows, numeric=[2, 3, 4],
                   note="One row per completed task; the subtype is the recorded operational outcome, not a correctness judgment."),
            _table("Task subtype totals", ["subtype", "count", "share"], _subtype_counts(raw_tasks),
                   numeric=[1, 2]),
            _table("Per-task shape", ["metric", "p50", "p90", "max", "n"], shape_rows,
                   numeric=[1, 2, 3, 4]),
            _table("Permission denials", ["source", "events", "tasks", "share of events"], denial_rows,
                   numeric=[1, 2, 3],
                   note="All denial sources are listed, including zero counts. The source is the only label-derived value shown."),
            _table("Tasks per session", ["tasks", "sessions", "share of sessions"], per_session_rows,
                   numeric=[0, 1, 2]),
            _table("Invocation summaries", ["session", "invocation", "started", "model", "project",
                                            "identity", "success", "duration", "turns", "error"],
                   summary_rows, numeric=[7, 8],
                   note="Error text is shown as recorded, bounded by cake's telemetry writer."),
            _table("Invocation summary failure reasons", ["error", "count"], error_rows,
                   numeric=[1]),
            _table("Turns per session", ["session", "invocations", "turns"], turn_rows,
                   numeric=[1, 2]),
            _table("Invocations without a session_summary", ["session", "invocation", "started",
                                                             "model", "project", "identity",
                                                             "attempts", "tool calls", "gate decisions",
                                                             "last activity"],
                   under_rows, numeric=[6, 7, 8],
                   note="These are crash, kill, or abandonment candidates. A live invocation may appear here if the report runs before it finishes."),
            _table("Sessions with an incomplete final task", ["session", "task", "started", "model",
                                                              "project", "identity", "last activity"],
                   inflight_rows,
                   note="The final task has no task_complete record. It is not counted in the completed-task totals."),
        ],
        notes=[
            "Task outcomes use transcripts and are attributed to task start. Invocation summaries use telemetry and their own timestamps.",
            "All rows are shown. Long tables scroll in the page.",
        ],
    )


# ---------------------------------------------------------------------------
# Tools
# ---------------------------------------------------------------------------

def _tool_failure_rows(calls: list[tuple[cakelib.Session, cakelib.ToolCall]]) -> tuple[list[list], int, int]:
    grouped = defaultdict(list)
    for _, call in calls:
        grouped[call.name].append(call)
    rows = []
    for tool, group in sorted(grouped.items(), key=lambda item: (-len(item[1]), item[0])):
        failures = sum(not call.ok for call in group)
        rows.append([tool, cakelib.fmt_int(len(group)), cakelib.fmt_int(failures),
                     _rate_cell(failures, len(group))])
    total = len(calls)
    failures = sum(not call.ok for _, call in calls)
    if rows:
        rows.append(["TOTAL", cakelib.fmt_int(total), cakelib.fmt_int(failures),
                     _rate_cell(failures, total)])
    return rows, total, failures


def _taxonomy_rows(calls: list[tuple[cakelib.Session, cakelib.ToolCall]]) -> list[list]:
    counts = Counter((call.name, cakelib.classify_tool_error(call.name, call.output))
                     for _, call in calls if not call.ok)
    total = sum(counts.values())
    return [[tool, category, cakelib.fmt_int(count), _rate_cell(count, total)]
            for (tool, category), count in counts.most_common()]


def _recovery_rows(data: cakelib.Dataset, start: datetime, end: datetime) -> list[list]:
    rows = []
    for tool in tools.FILE_TOOLS:
        retried = recovered = abandoned = 0
        for session in data.sessions:
            calls = session.tool_calls_in_window(start, end)
            for index, call in enumerate(calls):
                if call.name != tool or call.ok:
                    continue
                later = [other for other in calls[index + 1:]
                         if other.name in tools.FILE_TOOLS and other.path == call.path and call.path]
                if later:
                    retried += 1
                    recovered += any(other.ok for other in later)
                else:
                    abandoned += 1
        if retried or abandoned:
            rows.append([tool, cakelib.fmt_int(retried), cakelib.fmt_int(recovered),
                         _rate_cell(recovered, retried), cakelib.fmt_int(abandoned)])
    return rows


def _telemetry_tool_rows(data: cakelib.Dataset, start: datetime, end: datetime) -> list[list]:
    calls = [call for inv in data.invocations for call in inv.tool_calls
             if _in(call, inv.mtime, start, end)]
    grouped = defaultdict(list)
    for call in calls:
        grouped[call.get("name", "?")].append(call)
    rows = []
    for tool, group in sorted(grouped.items(), key=lambda item: (-len(item[1]), item[0])):
        durations = [call.get("duration_ms") for call in group
                     if call.get("duration_ms") is not None]
        sizes = [call.get("output_bytes") for call in group
                 if call.get("output_bytes") is not None]
        errors = sum(bool(call.get("was_error")) for call in group)
        rows.append([
            tool, cakelib.fmt_int(len(group)), cakelib.fmt_int(errors),
            _rate_cell(errors, len(group)),
            _duration(_p50(durations)), _duration(_p90(durations)),
            _duration(max(durations)) if durations else "-",
            cakelib.fmt_bytes(_p50(sizes)) if sizes else "-",
            cakelib.fmt_bytes(max(sizes)) if sizes else "-",
        ])
    return rows


def _parallelism_rows(data: cakelib.Dataset, start: datetime, end: datetime) -> list[list]:
    per_turn = Counter()
    for inv in data.invocations:
        calls = [call for call in inv.tool_calls if _in(call, inv.mtime, start, end)]
        for _, group in cakelib.group_by(calls, lambda call: call.get("turn_index")).items():
            per_turn[len(group)] += 1
    total = sum(per_turn.values())
    return [[count, cakelib.fmt_int(turns), _rate_cell(turns, total)]
            for count, turns in sorted(per_turn.items())]


def _slowest_tool_rows(data: cakelib.Dataset, start: datetime, end: datetime) -> list[list]:
    found = []
    for inv in data.invocations:
        for call in inv.tool_calls:
            moment = _record_ts(call, inv.mtime)
            if moment is None or not cakelib.ts_in_window(moment, start, end):
                continue
            duration = call.get("duration_ms") or 0
            found.append((duration, [
                call.get("name", "?"), inv.session_id, inv.invocation_id,
                cakelib.fmt_utc(moment), _duration(call.get("duration_ms")),
                cakelib.fmt_bytes(call.get("output_bytes") or 0),
                "yes" if call.get("was_error") else "no",
                call.get("turn_index", "?"), call.get("attempt", "?"),
            ]))
    found.sort(key=lambda item: item[0], reverse=True)
    return [row for _, row in found]


def _group_tool_failures(calls: list[tuple[cakelib.Session, cakelib.ToolCall]],
                         key_fn) -> list[list]:
    grouped = defaultdict(lambda: [0, 0])
    for session, call in calls:
        key = key_fn(session, call)
        grouped[key][0] += 1
        grouped[key][1] += not call.ok
    rows = []
    for key, (total, failures) in sorted(grouped.items(), key=lambda item: (-item[1][0], item[0])):
        rows.append([*key, cakelib.fmt_int(total), cakelib.fmt_int(failures),
                     _rate_cell(failures, total)])
    return rows


def _judge_rate_rows(rows: list[list]) -> list[list]:
    """Replace the formatted bare block/warn percentages with full rate cells."""
    count_end = 3 + len(tools.JUDGE_OUTCOMES)
    result = []
    for row in rows:
        counts = [int(str(value).replace(",", "")) for value in row[2:count_end]]
        _calls, allow, warn, block = counts[:4]
        judged = allow + warn + block
        result.append([*row[:-2], _rate_cell(block, judged), _rate_cell(warn, judged)])
    return result


def tools_section(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    calls = _window_calls(data, start, end)
    tool_rows, _, _ = _tool_failure_rows(calls)
    coverage = []
    bash_calls = [(session, call) for session, call in calls if call.name == "Bash"]
    for model, group in sorted(cakelib.group_by(bash_calls, lambda sc: sc[0].model).items(),
                               key=lambda item: (-len(item[1]), item[0])):
        with_reason = sum(bool(call.reason) for _, call in group)
        coverage.append([model, cakelib.fmt_int(len(group)), cakelib.fmt_int(with_reason),
                         _rate_cell(with_reason, len(group))])
    total_bash = len(bash_calls)
    total_reason = sum(bool(call.reason) for _, call in bash_calls)
    if coverage:
        coverage.append(["TOTAL", cakelib.fmt_int(total_bash), cakelib.fmt_int(total_reason),
                         _rate_cell(total_reason, total_bash)])

    judge_rows, linkage = tools.bash_judge_outcomes(data, start, end)
    judge_rows = _judge_rate_rows(judge_rows)
    judge_headers = ["model", "reason", "calls", *tools.JUDGE_OUTCOME_HEADERS,
                     "block %", "warn %"]
    linkage_rows = [
        ["Bash calls", cakelib.fmt_int(linkage["calls"])],
        ["paired judge outcomes", cakelib.fmt_int(linkage["paired"])],
        ["no call id", cakelib.fmt_int(linkage["no_call_id"])],
        ["no sidecar", cakelib.fmt_int(linkage["no_sidecar"])],
        ["no event", cakelib.fmt_int(linkage["no_event"])],
        ["ambiguous calls", cakelib.fmt_int(linkage["ambiguous"])],
        ["judge events", cakelib.fmt_int(linkage["events"])],
        ["events paired", cakelib.fmt_int(linkage["events_paired"])],
        ["events ambiguous", cakelib.fmt_int(linkage["events_ambiguous"])],
        ["events with no linkage key", cakelib.fmt_int(sum(linkage["events_keyless"].values()))],
        ["events whose call is outside the window",
         cakelib.fmt_int(sum(linkage["events_windowless"].values()))],
        ["judge attempts", cakelib.fmt_int(linkage["attempts"])],
        ["calls with judge attempts", cakelib.fmt_int(linkage["attempted_calls"])],
        ["calls with more than one judge attempt", cakelib.fmt_int(linkage["retried_calls"])],
    ]

    model_rows = _group_tool_failures(calls, lambda session, call: (session.model, call.name))
    project_rows = _group_tool_failures(calls, lambda session, call: (session.working_directory, call.name))
    collected = _collect_attribution(data, start, end)
    identity_rows = []
    for (identity, tool), stats in sorted(collected["tools"].items()):
        identity_rows.append([identity, tool, cakelib.fmt_int(stats["calls"]),
                              cakelib.fmt_int(stats["failures"]),
                              _rate_cell(stats["failures"], stats["calls"])])

    return _section(
        "tools", "Tool calls",
        "Every tool, every failure category, every Bash judge outcome, retry recovery, durations, output sizes, and the slowest individual calls.",
        tables=[
            _table("Per tool (transcripts)", ["tool", "calls", "failures", "failure rate"],
                   tool_rows, numeric=[1, 2, 3],
                   note="Every tool is shown. `TOTAL` is the flat total, not a top-N sum."),
            _table("Failure taxonomy", ["tool", "category", "count", "share of failures"],
                   _taxonomy_rows(calls), numeric=[2, 3],
                   note="Every category is shown, including `other` when a failure shape falls outside the classifier."),
            _table("Bash `reason` coverage", ["model", "calls", "with reason", "coverage"],
                   coverage, numeric=[1, 2, 3],
                   note="Presence of a parsed, non-empty `reason` only. The reason text never enters the report."),
            _table("Bash judge outcomes by reason presence", judge_headers, judge_rows,
                   numeric=list(range(2, len(judge_headers))),
                   note="Outcome columns sum to `calls`. A call with no judge outcome was not judged or was judged without a linked event."),
            _table("Judge linkage accounting", ["account", "count"], linkage_rows, numeric=[1],
                   note="This accounts for every Bash call and every judge event in the window. The raw command, reason, and call id never enter a row."),
            _table("Retry recovery after failure (Edit/Write, same path, same session)",
                   ["tool", "retried", "recovered", "recovery rate", "not retried"],
                   _recovery_rows(data, start, end), numeric=[1, 2, 3, 4]),
            _table("Durations and output sizes (telemetry)", ["tool", "calls", "errors", "error rate",
                                                              "dur p50", "dur p90", "dur max",
                                                              "out p50", "out max"],
                   _telemetry_tool_rows(data, start, end), numeric=[1, 2, 3, 4, 5, 6, 7, 8]),
            _table("Tool calls per assistant turn", ["calls in turn", "turns", "share"],
                   _parallelism_rows(data, start, end), numeric=[0, 1, 2]),
            _table("Tool failures by model", ["model", "tool", "calls", "failures", "failure rate"],
                   model_rows, numeric=[2, 3, 4]),
            _table("Tool failures by project", ["project", "tool", "calls", "failures", "failure rate"],
                   project_rows, numeric=[2, 3, 4]),
            _table("Tool failures by build identity", ["identity", "tool", "calls", "failures", "failure rate"],
                   identity_rows, numeric=[2, 3, 4]),
            _table("Tool calls (slowest first)", ["tool", "session", "invocation", "started",
                                                  "duration", "output", "error", "turn", "attempt"],
                   _slowest_tool_rows(data, start, end), numeric=[4, 5, 7, 8],
                   note="Telemetry tool calls in the window, sorted by duration."),
        ],
        notes=[
            "Transcript counts and telemetry counts are separate sources. A transcript call has no duration; a telemetry call has no paired output body. They are not silently merged.",
            "The Bash judge matrix joins by the SHA-256 digest of the transcript call id within one session. A retried judge call contributes one outcome.",
            "Tables above the dashboard detail limit are capped and state the full row count; raise --detail-limit for more.",
        ],
    )


# ---------------------------------------------------------------------------
# Assembly
# ---------------------------------------------------------------------------

def _apply_detail_limit(sections: list[dict], limit: int) -> list[dict]:
    """Cap every table at `limit` rows and record the full count in its note.

    A limit of zero or less leaves the tables untouched, which is the explicit
    opt-in for a full-detail page. The builders themselves never truncate.
    """
    if limit <= 0:
        return sections
    for section in sections:
        for table in section["tables"]:
            total = len(table["rows"])
            if total <= limit:
                continue
            table["rows"] = table["rows"][:limit]
            table["total_rows"] = total
            note = (f"Showing {limit:,} of {total:,} rows. "
                    "Raise --detail-limit for more.")
            table["note"] = f"{table['note']} {note}" if table.get("note") else note
    return sections


def build_report(data: cakelib.Dataset, start: datetime, end: datetime,
                 now: datetime, detail_limit: int = DEFAULT_DETAIL_LIMIT) -> dict:
    """Build every dashboard section for one half-open window."""
    from dashboard_sections import (
        api_section, cache_section, compensations_section, definitions_section,
        gates_section, hooks_section, judge_section, sessions_section,
        time_section, tokens_section,
    )

    headline = headline_metrics(data, now)
    headline_section = _section(
        "headline", "Headline metrics",
        "The last seven complete UTC days against the preceding seven. Every metric keeps its sample and denominator.",
        tables=[headline["table"]],
        notes=[
            "No metric is suppressed for a small sample. Use the sample column and the full detail sections to judge whether a difference is material.",
            "Today's partial activity is excluded from this comparison and appears in the detail window and daily tables.",
        ],
    )
    sections = [
        provenance_section(data, start, end, now),
        headline_section,
        attribution_section(data, start, end),
        outcomes_section(data, start, end),
        tools_section(data, start, end),
        api_section(data, start, end),
        tokens_section(data, start, end),
        cache_section(data, start, end),
        gates_section(data, start, end),
        judge_section(data, start, end),
        time_section(data, start, end),
        hooks_section(data, start, end),
        compensations_section(data, start, end),
        sessions_section(data, start, end),
        definitions_section(),
    ]
    sections = _apply_detail_limit(sections, detail_limit)
    return {
        "schema_version": 2,
        "measured": cakelib.fmt_utc(now),
        "window": {"start": cakelib.fmt_utc(start), "end": cakelib.fmt_utc(end)},
        "headline": headline,
        "attribution": change_attribution(data, start, end),
        "sections": sections,
    }
