#!/usr/bin/env python3
"""One machine-readable summary of session metrics, with window comparison.

`summary.py` is the shared definition behind `report.py --json`,
`report.py --baseline`, and `report.py --vs`: a flat metric vocabulary, a
schema-versioned document with explicit provenance, and a pure `compare`
function. It reads the `cakelib.Dataset` the report already loaded; it never
reads files or telemetry itself.

Windows are half-open UTC intervals `[start, end)`. A `None` start means all
time. Window membership follows each record's own timestamp, with the source
file's mtime as the fallback, matching the rest of the suite. `measured` is when
the summary was taken; counts and parse errors come from the loaded source
directories, not from the window.

Every metric key is always present. An undefined rate or percentile is `null`,
never omitted, so a saved document keeps its shape across empty windows.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone

import cakelib

SCHEMA_VERSION = 1


@dataclass(frozen=True)
class Metric:
    """One documented key, its unit, and the direction the gate calls better."""

    key: str
    unit: str  # "rate" or "ms"
    higher_is_better: bool


METRICS = (
    Metric("outcomes.task_success_rate", "rate", True),
    Metric("outcomes.abnormal_termination_rate", "rate", False),
    Metric("tools.edit.failure_rate", "rate", False),
    Metric("tools.edit.no_match_rate", "rate", False),
    Metric("tokens.cache_hit_rate", "rate", True),
    Metric("api.latency.p95", "ms", False),
    Metric("api.failure_rate", "rate", False),
    Metric("judge.timeout_rate", "rate", False),
    Metric("judge.failure_rate", "rate", False),
    Metric("judge.block_rate", "rate", False),
)
METRIC_BY_KEY = {metric.key: metric for metric in METRICS}
METRIC_KEYS = tuple(metric.key for metric in METRICS)


def summarize(
    data: cakelib.Dataset, start: datetime | None, end: datetime | None,
    *, measured: datetime | None = None, filters: dict | None = None,
) -> dict:
    """Build the schema-versioned summary for one half-open window.

    `start` and `end` are UTC bounds; `None` is unbounded. `filters` is the
    report's provenance record (model/project/session/invocation), not an
    instruction: `cakelib.load` has already applied it.
    """
    measured = measured or datetime.now(timezone.utc)
    filters = dict(filters or {})

    tasks, tool_calls, edit_calls = [], 0, []
    for session in data.sessions:
        tasks.extend(session.tasks_in_window(start, end=end))
        calls = session.tool_calls_in_window(start, end=end)
        tool_calls += len(calls)
        edit_calls.extend(call for call in calls if call.name == "Edit")

    api_attempts = _invocation_records(data.invocations, "attempts", start, end)
    judge_attempts = _invocation_records(data.invocations, "judge_attempts", start, end)
    judge_verdicts = [record for invocation in data.invocations
                      for record in invocation.compensations
                      if record.get("kind") == "judge_verdict"
                      and _in_window(record, invocation.mtime, start, end)]
    started = [invocation for invocation in data.invocations
               if invocation.init is not None
               and _in_window(invocation.init, invocation.mtime, start, end)]

    successes = sum(task.get("subtype") == "success" for task in tasks)
    edit_failures = [call for call in edit_calls if not call.ok]
    no_match = sum(cakelib.classify_tool_error(call.name, call.output)
                   == cakelib.NO_MATCH_CATEGORY for call in edit_failures)
    latencies = [attempt["request_ms"] for attempt in api_attempts
                 if attempt.get("request_ms") is not None]
    api_failures = sum(bool(attempt.get("error")
                            or attempt.get("status_code") not in (200, None))
                       for attempt in api_attempts)
    cache_input = cached_input = 0
    for attempt in api_attempts:
        usage = attempt.get("usage") or {}
        cached = (usage.get("input_tokens_details") or {}).get("cached_tokens")
        if cached is not None:
            cache_input += usage.get("input_tokens") or usage.get("prompt_tokens") or 0
            cached_input += cached

    outcomes = [cakelib.judge_event_outcome(event) for event in judge_verdicts]
    values = {
        "outcomes.task_success_rate": _rate(successes, len(tasks)),
        "outcomes.abnormal_termination_rate": _rate(
            sum(invocation.summary is None for invocation in started), len(started)),
        "tools.edit.failure_rate": _rate(len(edit_failures), len(edit_calls)),
        "tools.edit.no_match_rate": _rate(no_match, len(edit_calls)),
        "tokens.cache_hit_rate": _rate(cached_input, cache_input),
        "api.latency.p95": cakelib.percentile(latencies, 95) if latencies else None,
        "api.failure_rate": _rate(api_failures, len(api_attempts)),
        "judge.timeout_rate": _rate(
            sum(attempt.get("terminal_class") == "timeout" for attempt in judge_attempts),
            len(judge_attempts)),
        "judge.failure_rate": _rate(
            sum(attempt.get("terminal_class") != "verdict" for attempt in judge_attempts),
            len(judge_attempts)),
        "judge.block_rate": _rate(
            outcomes.count("block"),
            sum(outcome in cakelib.JUDGE_VERDICTS for outcome in outcomes)),
    }

    return {
        "schema_version": SCHEMA_VERSION,
        "measured": cakelib.fmt_utc(measured),
        "window": {"start": cakelib.fmt_utc(start) if start is not None else None,
                   "end": cakelib.fmt_utc(end) if end is not None else None},
        "filters": filters,
        "counts": {
            "sessions": sum(s.active_between(start, end) for s in data.sessions),
            "invocations": sum(i.active_between(start, end) for i in data.invocations),
            "invocations_started": len(started),
            "tasks": len(tasks),
            "tasks_succeeded": successes,
            "tool_calls": tool_calls,
            "edit_calls": len(edit_calls),
            "api_attempts": len(api_attempts),
            "judge_attempts": len(judge_attempts),
            "judge_verdicts": len(judge_verdicts),
            "session_parse_errors": data.session_parse_errors,
            "telemetry_parse_errors": data.telemetry_parse_errors,
        },
        "metric_metadata": {m.key: {"unit": m.unit,
                                    "higher_is_better": m.higher_is_better}
                            for m in METRICS},
        "metrics": {metric.key: values[metric.key] for metric in METRICS},
    }


def compare(current: dict, baseline: dict) -> list[dict]:
    """Compare two summary documents key by key.

    Returns one row per documented metric with `value`, `baseline`, `delta`,
    and `direction`. A key missing from the baseline, or `null` on either side,
    yields a `null` delta and the direction `n/a`; it is never treated as zero.
    """
    baseline_metrics = baseline.get("metrics")
    if not isinstance(baseline_metrics, dict):
        baseline_metrics = {}
    rows = []
    for metric in METRICS:
        value = _number(_metric_value(current, metric.key))
        reference = _number(baseline_metrics.get(metric.key))
        delta = value - reference if value is not None and reference is not None else None
        rows.append({"key": metric.key, "value": value, "baseline": reference,
                     "delta": delta,
                     "direction": _direction(delta, metric.higher_is_better)})
    return rows


def missing_keys(document: dict) -> list[str]:
    """Documented keys absent from a summary document, in vocabulary order."""
    metrics = document.get("metrics")
    if not isinstance(metrics, dict):
        return list(METRIC_KEYS)
    return [key for key in METRIC_KEYS if key not in metrics]


def _direction(delta: float | None, higher_is_better: bool) -> str:
    if delta is None:
        return "n/a"
    return "unchanged" if delta == 0 else (
        "better" if (delta > 0) == higher_is_better else "worse")


def _metric_value(document: dict, key: str) -> object:
    metrics = document.get("metrics")
    return metrics.get(key) if isinstance(metrics, dict) else None


def _number(value: object) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return float(value)


def _rate(numerator: int, denominator: int) -> float | None:
    return numerator / denominator if denominator else None


def _in_window(record: dict, fallback: datetime | None,
               start: datetime | None, end: datetime | None) -> bool:
    ts = cakelib.parse_ts(record.get("timestamp")) or fallback
    return cakelib.ts_in_window(ts, start, end)


def _invocation_records(invocations: list[cakelib.Invocation], attribute: str,
                       start: datetime | None, end: datetime | None) -> list[dict]:
    return [record for invocation in invocations
            for record in getattr(invocation, attribute)
            if _in_window(record, invocation.mtime, start, end)]
