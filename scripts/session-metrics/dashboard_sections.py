"""Full-detail dashboard sections: API, tokens, cache, gates, judge, time, hooks, compensations, and sessions.

The builders are split from `dashboard_report.py` to keep each module
reviewable. Shared display and window helpers stay in `dashboard_report.py`.
"""

from __future__ import annotations

from collections import Counter, defaultdict
from datetime import datetime
from statistics import median

import api
import cache_breaks
import cakelib
import compensations
import gates
import judge
import time_breakdown
import tools

from dashboard_report import (
    _build_intervals,
    _duration,
    _identity_at,
    _in,
    _p50,
    _p90,
    _p99,
    _rate_cell,
    _record_ts,
    _section,
    _table,
    _window_attempts,
    _window_gate_decisions,
    _window_gate_startups,
    _window_retries,
    _window_tasks,
)


# ---------------------------------------------------------------------------
# API reliability
# ---------------------------------------------------------------------------

def _window_dataset(data: cakelib.Dataset, start: datetime, end: datetime) -> cakelib.Dataset:
    """The same dataset with only invocations active in the window.

    Aggregate helpers that are not window-aware still see only the invocations
    that overlap the report, so a narrower caller cannot silently include old
    telemetry.
    """
    return cakelib.Dataset(
        sessions=list(data.sessions),
        invocations=[inv for inv in data.invocations if inv.active_between(start, end)],
        sessions_dir=data.sessions_dir,
        telemetry_dir=data.telemetry_dir,
        cutoff=data.cutoff,
        session_parse_errors=data.session_parse_errors,
        telemetry_parse_errors=data.telemetry_parse_errors,
        selected_invocation=data.selected_invocation,
        invocation_filter=data.invocation_filter,
    )


def _api_failure(attempt: dict) -> bool:
    return bool(attempt.get("error") or attempt.get("status_code") not in (200, None))


def _api_rows(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    attempts = _window_attempts(data, start, end)
    retries = _window_retries(data, start, end)
    statuses = Counter(attempt.get("status_code") for _, attempt in attempts)
    errors = Counter((attempt.get("error") or "(none)")
                     for _, attempt in attempts if _api_failure(attempt))
    terminations = Counter((attempt.get("termination") or {}).get("classification", "unknown")
                           for _, attempt in attempts)
    by_model = defaultdict(list)
    for inv, attempt in attempts:
        by_model[inv.model].append(attempt)
    latency_rows = []
    for model, group in sorted(by_model.items(), key=lambda item: (-len(item[1]), item[0])):
        latencies = [attempt["request_ms"] for attempt in group
                     if attempt.get("request_ms") is not None]
        latency_rows.append([
            model, cakelib.fmt_int(len(group)),
            _duration(_p50(latencies)), _duration(_p90(latencies)),
            _duration(_p99(latencies)), _duration(max(latencies)) if latencies else "-",
            cakelib.fmt_int(len(latencies)),
        ])
    retry_reasons = api._retry_reason_rows([retry for _, retry in retries])
    retry_rows = [[row["reason"], cakelib.fmt_int(row["count"]),
                   _duration(row["delay_p50"]), _duration(row["delay_max"]),
                   cakelib.fmt_int(row["deepest"]), cakelib.fmt_int(row["max_retries"]),
                   cakelib.fmt_int(row["changed_overrides"])]
                  for row in retry_reasons]
    per_invocation = Counter(
        sum(1 for attempt in inv.attempts if _in(attempt, inv.mtime, start, end))
        for inv in data.invocations
        if any(_in(attempt, inv.mtime, start, end) for attempt in inv.attempts)
    )
    invocation_rows = [[count, cakelib.fmt_int(invocations),
                        _rate_cell(invocations, sum(per_invocation.values()))]
                       for count, invocations in sorted(per_invocation.items())]
    found = []
    for inv, attempt in attempts:
        moment = _record_ts(attempt, inv.mtime)
        usage = attempt.get("usage") or {}
        total = attempt.get("total_ms") or 0
        status = attempt.get("status_code")
        found.append((total, [
            inv.session_id, inv.invocation_id, inv.model, inv.working_directory,
            inv.build_identity, moment and cakelib.fmt_utc(moment) or "-",
            attempt.get("turn_index", "?"), attempt.get("attempt", "?"),
            status if status is not None else "(none)",
            "yes" if _api_failure(attempt) else "no",
            _duration(attempt.get("total_ms")),
            _duration(attempt.get("request_ms")),
            cakelib.fmt_int(usage.get("input_tokens") or usage.get("prompt_tokens") or 0),
            cakelib.fmt_int(usage.get("output_tokens") or usage.get("completion_tokens") or 0),
            (attempt.get("error") or "-"),
        ]))
    found.sort(key=lambda item: item[0], reverse=True)
    attempt_rows = [row for _, row in found]
    return {
        "attempts": attempts,
        "retries": retries,
        "status_rows": [[code if code is not None else "(none)", cakelib.fmt_int(count),
                         _rate_cell(count, len(attempts))]
                        for code, count in statuses.most_common()],
        "error_rows": [[error, cakelib.fmt_int(count)]
                       for error, count in errors.most_common()],
        "termination_rows": [[name, cakelib.fmt_int(count),
                              _rate_cell(count, len(attempts))]
                             for name, count in terminations.most_common()],
        "latency_rows": latency_rows,
        "retry_rows": retry_rows,
        "invocation_rows": invocation_rows,
        "attempt_rows": attempt_rows,
        "failure_count": sum(1 for _, attempt in attempts if _api_failure(attempt)),
        "retry_count": len(retries),
    }


def _api_failures_by(attempts: list[tuple[cakelib.Invocation, dict]], key_fn) -> list[list]:
    grouped = defaultdict(lambda: [0, 0])
    for inv, attempt in attempts:
        key = key_fn(inv, attempt)
        grouped[key][0] += 1
        grouped[key][1] += _api_failure(attempt)
    rows = []
    for key, (total, failures) in sorted(grouped.items(), key=lambda item: (-item[1][0], item[0])):
        rows.append([*key, cakelib.fmt_int(total), cakelib.fmt_int(failures),
                     _rate_cell(failures, total)])
    return rows


def api_section(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    facts = _api_rows(data, start, end)
    attempts = facts["attempts"]
    failed = facts["failure_count"]
    retries = facts["retry_count"]
    overflow = sum(1 for _, attempt in attempts
                   if (attempt.get("request_overrides") or {}).get("context_overflow_retry_used"))
    return _section(
        "api", "API reliability",
        "Every provider attempt, status code, error variant, termination class, retry reason, and latency sample.",
        cards=[
            {"label": "Attempts", "value": cakelib.fmt_int(len(attempts)),
             "detail": f"failed {_rate_cell(failed, len(attempts))}"},
            {"label": "Retries", "value": cakelib.fmt_int(retries),
             "detail": f"retry rate {_rate_cell(retries, len(attempts))}"},
            {"label": "Context-overflow overrides", "value": cakelib.fmt_int(overflow),
             "detail": "attempts sent with the override active"},
        ],
        tables=[
            _table("Status codes", ["status", "attempts", "share"], facts["status_rows"],
                   numeric=[1, 2]),
            _table("Error variants", ["error", "count"], facts["error_rows"], numeric=[1],
                   note="Error text is shown as recorded, with no top-N cut."),
            _table("Termination classification", ["classification", "attempts", "share"],
                   facts["termination_rows"], numeric=[1, 2]),
            _table("Latency by model (request_ms)", ["model", "attempts", "p50", "p90", "p99", "max", "n"],
                   facts["latency_rows"], numeric=[1, 2, 3, 4, 5, 6]),
            _table("Retry reasons", ["reason", "count", "delay p50", "delay max", "deepest attempt",
                                     "max retries", "changed overrides"],
                   facts["retry_rows"], numeric=[1, 2, 3, 4, 5, 6]),
            _table("Attempts per invocation", ["attempts", "invocations", "share"],
                   facts["invocation_rows"], numeric=[0, 1, 2]),
            _table("API failures by model", ["model", "attempts", "failures", "failure rate"],
                   _api_failures_by(attempts, lambda inv, attempt: (inv.model,)),
                   numeric=[1, 2, 3]),
            _table("API failures by project", ["project", "attempts", "failures", "failure rate"],
                   _api_failures_by(attempts, lambda inv, attempt: (inv.working_directory,)),
                   numeric=[1, 2, 3]),
            _table("API failures by build identity", ["identity", "attempts", "failures", "failure rate"],
                   _api_failures_by(attempts, lambda inv, attempt: (inv.build_identity,)),
                   numeric=[1, 2, 3]),
            _table("Every API attempt (slowest first)", ["session", "invocation", "model", "project",
                                                         "identity", "started", "turn", "attempt",
                                                         "status", "failed", "total", "request",
                                                         "input", "output", "error"],
                   facts["attempt_rows"], numeric=[6, 7, 8, 10, 11, 12, 13],
                   note="All provider attempts in the window, sorted by total time. This table scrolls rather than truncating."),
        ],
        notes=[
            "API failure means a recorded error or a status other than 200. A missing status is not a failure by itself.",
            "Latency uses `request_ms`; total time includes response parsing and is shown on the individual attempt rows.",
            "Retry delay is the scheduled delay, not a measured wait unless a `retry_wait` record exists.",
        ],
    )


# ---------------------------------------------------------------------------
# Tokens and cache
# ---------------------------------------------------------------------------

USAGE_HEADERS = ["", "input", "cached", "cache write", "cache %", "output", "reasoning", "total"]


def _usage_row(label: str, usages: list[dict]) -> list:
    totals = cakelib.usage_totals(usages)
    return [label, cakelib.fmt_int(totals["input"]), cakelib.fmt_int(totals["cached"]),
            cakelib.fmt_int(totals["cache_write"]),
            _rate_cell(totals["cached"], totals["input"]),
            cakelib.fmt_int(totals["output"]), cakelib.fmt_int(totals["reasoning"]),
            cakelib.fmt_int(totals["total"])]


def _usage_rows(grouped: dict[str, list[dict]]) -> list[list]:
    return [_usage_row(label, usages) for label, usages in
            sorted(grouped.items(), key=lambda item: -sum(
                (usage.get("total_tokens") or 0) for usage in item[1]))]


def tokens_section(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    attempts = [(inv, attempt) for inv, attempt in _window_attempts(data, start, end)
                if attempt.get("usage")]
    usages = [attempt["usage"] for _, attempt in attempts]
    by_model = defaultdict(list)
    by_project = defaultdict(list)
    by_day = defaultdict(list)
    by_identity = defaultdict(list)
    for inv, attempt in attempts:
        by_model[inv.model].append(attempt["usage"])
        by_project[inv.working_directory].append(attempt["usage"])
        moment = _record_ts(attempt, inv.mtime)
        if moment:
            by_day[moment.date().isoformat()].append(attempt["usage"])
        by_identity[inv.build_identity].append(attempt["usage"])

    inv_rows = []
    for inv in data.invocations:
        if not inv.summary or not _in(inv.summary, inv.mtime, start, end):
            continue
        usage = inv.summary.get("usage") or {}
        if not usage:
            continue
        inv_rows.append([
            inv.session_id, inv.invocation_id, inv.model, inv.build_identity,
            cakelib.fmt_int(inv.summary.get("turn_count") or 0),
            cakelib.fmt_int(usage.get("input_tokens") or usage.get("prompt_tokens") or 0),
            cakelib.fmt_int((usage.get("input_tokens_details") or {}).get("cached_tokens") or 0),
            cakelib.fmt_int((usage.get("input_tokens_details") or {}).get("cache_write_tokens") or 0),
            cakelib.fmt_int(usage.get("output_tokens") or usage.get("completion_tokens") or 0),
            cakelib.fmt_int((usage.get("output_tokens_details") or {}).get("reasoning_tokens") or 0),
            cakelib.fmt_int(usage.get("total_tokens") or 0),
        ])
    inv_totals = [int(row[10].replace(",", "")) for row in inv_rows]
    context_inputs = [attempt["usage"].get("input_tokens") or 0 for _, attempt in attempts]
    history_items = [attempt.get("history_items") or 0 for _, attempt in attempts]

    task_usages = [task["usage"] for _, task, _ in _window_tasks(data, start, end)
                   if task.get("usage")]
    task_count = len(_window_tasks(data, start, end))
    totals = cakelib.usage_totals(usages)
    per_task = ((totals["input"] + totals["output"]) / task_count
                if task_count else None)
    return _section(
        "tokens", "Token usage and cache",
        "Provider-reported usage from every API attempt, cross-checked against transcript totals, with per-invocation detail.",
        cards=[
            {"label": "Input tokens", "value": cakelib.fmt_int(totals["input"]),
             "detail": f"cache hit {_rate_cell(totals['cached'], totals['input'])}"},
            {"label": "Output tokens", "value": cakelib.fmt_int(totals["output"]),
             "detail": f"reasoning {cakelib.fmt_int(totals['reasoning'])}"},
            {"label": "Tokens per completed task",
             "value": cakelib.fmt_int(per_task) if per_task is not None else "-",
             "detail": f"across {cakelib.fmt_int(task_count)} tasks"},
        ],
        tables=[
            _table("Totals (telemetry api_attempt)", USAGE_HEADERS,
                   [_usage_row("all models", usages)], numeric=list(range(1, 8))),
            _table("By model", USAGE_HEADERS, _usage_rows(by_model), numeric=list(range(1, 8))),
            _table("By project", USAGE_HEADERS, _usage_rows(by_project), numeric=list(range(1, 8))),
            _table("By day", USAGE_HEADERS,
                   [_usage_row(day, by_day[day]) for day in sorted(by_day)],
                   numeric=list(range(1, 8))),
            _table("By build identity", USAGE_HEADERS, _usage_rows(by_identity), numeric=list(range(1, 8))),
            _table("Per-invocation totals (session_summary)", ["session", "invocation", "model",
                                                               "identity", "turns", "input", "cached",
                                                               "cache write", "output", "reasoning", "total"],
                   inv_rows, numeric=[4, 5, 6, 7, 8, 9, 10]),
            _table("Per-invocation total distribution", ["metric", "p50", "p90", "max", "n"],
                   [["total tokens", cakelib.fmt_int(_p50(inv_totals) or 0),
                     cakelib.fmt_int(_p90(inv_totals) or 0),
                     cakelib.fmt_int(max(inv_totals)) if inv_totals else "-",
                     cakelib.fmt_int(len(inv_totals))]], numeric=[1, 2, 3, 4]),
            _table("Context growth (per api_attempt)", ["metric", "p50", "p90", "max", "n"],
                   [
                       ["input tokens/request", cakelib.fmt_int(_p50(context_inputs) or 0),
                        cakelib.fmt_int(_p90(context_inputs) or 0),
                        cakelib.fmt_int(max(context_inputs)) if context_inputs else "-",
                        cakelib.fmt_int(len(context_inputs))],
                       ["history items/request", cakelib.fmt_int(_p50(history_items) or 0),
                        cakelib.fmt_int(_p90(history_items) or 0),
                        cakelib.fmt_int(max(history_items)) if history_items else "-",
                        cakelib.fmt_int(len(history_items))],
                   ], numeric=[1, 2, 3, 4]),
            _table("Transcript cross-check (task_complete/result usage)", USAGE_HEADERS,
                   [_usage_row("all sessions", task_usages)] if task_usages else [],
                   numeric=list(range(1, 8)),
                   note="Transcript totals cover sessions without telemetry. They are a cross-check, not an additive total."),
        ],
        notes=[
            "Cache percentage in this table is cached tokens over all reported input tokens. The headline cache-hit rate uses only requests that reported cached-token details; that narrower denominator is printed there.",
            "`total_tokens` is provider-reported. If a provider reports partial usage, the missing counter is absent rather than inferred.",
        ],
    )


def cache_section(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    scan = cache_breaks.scan([inv for inv in data.invocations if inv.active_between(start, end)])
    sessions = {session.id: session for session in data.sessions}
    intervals = _build_intervals(data)
    causes = defaultdict(list)
    rows = []
    for miss in scan.misses:
        causes[miss.cause].append(miss)
        session = sessions.get(miss.session_id)
        moment = cakelib.parse_ts(miss.timestamp)
        identity = (_identity_at(session, moment, intervals)
                    if session else "unknown")
        rows.append([miss.session_id, miss.invocation_id, identity, miss.model,
                     miss.cause, cakelib.fmt_int(miss.missed_tokens),
                     _duration(miss.idle_ms), miss.timestamp or "-",
                     cakelib.fmt_int(miss.turn_index)])
    cause_rows = [[cause, cakelib.fmt_int(len(misses)),
                   cakelib.fmt_int(sum(miss.missed_tokens for miss in misses)),
                   _rate_cell(len(misses), scan.miss_count)] for cause, misses
                  in sorted(causes.items(), key=lambda item: -len(item[1]))]
    return _section(
        "cache", "Prompt-cache breaks",
        "Detected cache misses above the 1,024-token noise floor, with the likely cause and every individual miss.",
        cards=[
            {"label": "Misses", "value": cakelib.fmt_int(scan.miss_count), "detail": "above the noise floor"},
            {"label": "Missed prompt tokens", "value": cakelib.fmt_int(scan.missed_tokens),
             "detail": "provider-reported"},
            {"label": "Wasted cost", "value": "unavailable", "detail": "no pricing source"},
        ],
        tables=[
            _table("Misses by likely cause", ["cause", "misses", "missed tokens", "share"],
                   cause_rows, numeric=[1, 2, 3]),
            _table("Every cache miss", ["session", "invocation", "identity", "model", "cause",
                                        "missed tokens", "idle", "timestamp", "turn"],
                   rows, numeric=[5, 6, 8],
                   note="All detected misses in the loaded window. The scan never reads prompt text."),
        ],
        notes=[
            "The scan orders usage-bearing turns per session. It latches on the first cache activity, so a provider that never reports cache data does not produce false misses.",
            "Dollar cost is unavailable until a provider pricing source exists; no estimate is fabricated.",
        ],
    )


# ---------------------------------------------------------------------------
# Policy gates and historical judge
# ---------------------------------------------------------------------------

def _gate_table_row(gate: str, stats: dict, totals: dict | None = None) -> list:
    if totals is not None:
        stats = totals
    outcomes = stats["outcomes"]
    latency = stats["latency"]
    failures = outcomes.get("failure", 0) + outcomes.get("disabled", 0)
    judged = sum(outcomes.get(key, 0) for key in cakelib.GATE_DECISIONS)
    denominator = stats["decisions"] - outcomes.get("bypass", 0)
    return [
        gate,
        cakelib.fmt_int(stats["startups"]),
        cakelib.fmt_int(stats["startup_failures"]),
        cakelib.fmt_int(stats["decisions"]),
        cakelib.fmt_int(outcomes.get("allow", 0)),
        cakelib.fmt_int(outcomes.get("warn", 0)),
        cakelib.fmt_int(outcomes.get("block", 0)),
        cakelib.fmt_int(outcomes.get("abstain", 0)),
        cakelib.fmt_int(failures),
        cakelib.fmt_int(outcomes.get("bypass", 0)),
        cakelib.fmt_int(stats["evidence"]),
        _rate_cell(failures, denominator),
        _rate_cell(outcomes.get("block", 0), judged),
        _duration(_p50(latency)),
        _duration(_p90(latency)),
    ]


def _gate_dimension_rows(data: cakelib.Dataset, start: datetime, end: datetime,
                         dimension: str) -> list[list]:
    grouped = defaultdict(lambda: {"decisions": 0, "outcomes": Counter()})
    for inv, decision in _window_gate_decisions(data, start, end):
        gate = decision.get("gate") or "(bypass)"
        key = (gate, getattr(inv, dimension))
        entry = grouped[key]
        entry["decisions"] += 1
        entry["outcomes"][cakelib.gate_decision_outcome(decision)] += 1
    rows = []
    for (gate, value), entry in sorted(grouped.items()):
        outcomes = entry["outcomes"]
        failures = outcomes.get("failure", 0) + outcomes.get("disabled", 0)
        judged = sum(outcomes.get(key, 0) for key in cakelib.GATE_DECISIONS)
        denominator = entry["decisions"] - outcomes.get("bypass", 0)
        rows.append([gate, value, cakelib.fmt_int(entry["decisions"]),
                     cakelib.fmt_int(outcomes.get("allow", 0)),
                     cakelib.fmt_int(outcomes.get("warn", 0)),
                     cakelib.fmt_int(outcomes.get("block", 0)),
                     cakelib.fmt_int(outcomes.get("abstain", 0)),
                     cakelib.fmt_int(failures), cakelib.fmt_int(outcomes.get("bypass", 0)),
                     _rate_cell(failures, denominator),
                     _rate_cell(outcomes.get("block", 0), judged)])
    return rows


def gates_section(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    stats = gates.aggregate(_window_dataset(data, start, end))
    rows = []
    totals = {"startups": 0, "startup_failures": 0, "decisions": 0, "outcomes": Counter(),
              "evidence": 0, "latency": [], "failure_classes": Counter()}
    for gate in sorted(stats):
        entry = stats[gate]
        rows.append(_gate_table_row(gate, entry))
        for key in ("startups", "startup_failures", "decisions", "evidence"):
            totals[key] += entry[key]
        totals["outcomes"].update(entry["outcomes"])
        totals["latency"].extend(entry["latency"])
        totals["failure_classes"].update(entry["failure_classes"])
    if stats:
        rows.append(_gate_table_row("TOTAL", totals))

    startup_modes = Counter(((startup.get("gate") or "(unknown)"),
                             startup.get("mode", "?"), startup.get("outcome", "?"))
                            for inv, startup in _window_gate_startups(data, start, end))
    startup_rows = [[gate, mode, outcome, cakelib.fmt_int(count)]
                    for (gate, mode, outcome), count in startup_modes.most_common()]
    failure_rows = [[name, cakelib.fmt_int(count)]
                    for name, count in totals["failure_classes"].most_common()]
    decision_modes = Counter(((decision.get("gate") or "(bypass)"), decision.get("mode", "?"),
                              cakelib.gate_decision_outcome(decision))
                             for inv, decision in _window_gate_decisions(data, start, end))
    mode_rows = [[gate, mode, outcome, cakelib.fmt_int(count)]
                 for (gate, mode, outcome), count in decision_modes.most_common()]
    evidence_counts = Counter((evidence.get("gate") or "(unknown)", evidence.get("kind", "?"))
                              for inv in data.invocations for evidence in inv.gate_evidence
                              if _in(evidence, inv.mtime, start, end))
    evidence_rows = [[gate, kind, cakelib.fmt_int(count)]
                     for (gate, kind), count in evidence_counts.most_common()]

    return _section(
        "gates", "Policy gates",
        "Every generic gate startup, decision, outcome, failure class, and evidence count. Gate evidence payload data is counted, never printed.",
        cards=[
            {"label": "Gate startups", "value": cakelib.fmt_int(totals["startups"]),
             "detail": f"startup failures {cakelib.fmt_int(totals['startup_failures'])}"},
            {"label": "Gate decisions", "value": cakelib.fmt_int(totals["decisions"]),
             "detail": f"blocks {cakelib.fmt_int(totals['outcomes'].get('block', 0))}"},
            {"label": "Failures / disabled", "value": cakelib.fmt_int(
                totals["outcomes"].get("failure", 0) + totals["outcomes"].get("disabled", 0)),
             "detail": "fail-closed or unavailable gate"},
        ],
        tables=[
            _table("Per gate", ["gate", "startups", "startup failures", "decisions", "allow",
                                "warn", "block", "abstain", "failures/disabled", "bypass",
                                "evidence", "failure rate", "block rate", "p50", "p90"],
                   rows, numeric=list(range(1, 15)),
                   note="Every gate is shown. Failure rate is failures/disabled over decisions excluding bypass; block rate is block over allow+warn+block+abstain."),
            _table("Startups by mode and outcome", ["gate", "mode", "outcome", "count"],
                   startup_rows, numeric=[3]),
            _table("Decisions by mode and outcome", ["gate", "mode", "outcome", "count"],
                   mode_rows, numeric=[3]),
            _table("Failures by class", ["class", "events"], failure_rows, numeric=[1]),
            _table("Evidence counts by gate and kind", ["gate", "kind", "events"],
                   evidence_rows, numeric=[2],
                   note="Only the gate, kind, and count are reported. Gate evidence payload data can contain provider or command text, so it is never rendered."),
            _table("Gate outcomes by model", ["gate", "model", "decisions", "allow", "warn",
                                              "block", "abstain", "failures/disabled", "bypass",
                                              "failure rate", "block rate"],
                   _gate_dimension_rows(data, start, end, "model"), numeric=list(range(2, 11))),
            _table("Gate outcomes by build identity", ["gate", "identity", "decisions", "allow",
                                                       "warn", "block", "abstain",
                                                       "failures/disabled", "bypass",
                                                       "failure rate", "block rate"],
                   _gate_dimension_rows(data, start, end, "build_identity"), numeric=list(range(2, 11))),
        ],
        notes=[
            "A bypass record has no gate name; it appears as `(bypass)` and is excluded from the failure-rate denominator.",
            "A gate startup failure is reported separately from a decision failure. Both remain visible.",
        ],
    )


def _judge_rows(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    by_model = judge.aggregate(_window_dataset(data, start, end))
    attempts = [attempt for stats in by_model.values() for attempt in stats["attempts"]]
    terminal_totals = Counter()
    for stats in by_model.values():
        terminal_totals.update(stats["terminal"])
    model_rows = []
    for model in sorted(by_model):
        stats = by_model[model]
        latencies = stats["latency"]
        model_rows.append([
            model, cakelib.fmt_int(len(latencies)),
            *[cakelib.fmt_int(stats["terminal"].get(cls, 0)) for cls in judge.TERMINAL_CLASSES],
            _duration(_p50(latencies)), _duration(_p90(latencies)),
            _duration(_p99(latencies)), _duration(max(latencies)) if latencies else "-",
        ])
    phase_rows = []
    for model in sorted(by_model):
        stats = by_model[model]
        cells = [model]
        for phase in judge.PHASES:
            values = stats["phases"][phase]
            cells.append(f"{_duration(_p50(values))} / {_duration(_p90(values))}")
        phase_rows.append(cells)
    usage_rows = []
    for model in sorted(by_model):
        stats = by_model[model]
        usage = cakelib.usage_totals(stats["usage"])
        usage_rows.append([model, cakelib.fmt_int(len(stats["usage"])),
                           cakelib.fmt_int(usage["input"]), cakelib.fmt_int(usage["cached"]),
                           cakelib.fmt_int(usage["output"]), cakelib.fmt_int(usage["reasoning"]),
                           cakelib.fmt_int(usage["total"])])
    retries = [attempt for stats in by_model.values() for attempt in stats["retry_attempts"]]
    retry_rows = [[attempt.get("retry_reason", "?"), cakelib.fmt_int(count)]
                  for attempt, count in Counter(attempt.get("retry_reason", "?")
                                               for attempt in retries).most_common()]
    delays = [attempt.get("retry_delay_ms", 0) for attempt in retries]
    ratios = [ratio for stats in by_model.values() for ratio in stats["deadline_ratio"]]
    near_timeout = sum(stats["near_timeout"] for stats in by_model.values())
    statuses = Counter()
    for stats in by_model.values():
        statuses.update(stats["status"])
    return {
        "by_model": by_model,
        "attempts": attempts,
        "terminal_totals": terminal_totals,
        "model_rows": model_rows,
        "phase_rows": phase_rows,
        "usage_rows": usage_rows,
        "retry_rows": retry_rows,
        "delays": delays,
        "ratios": ratios,
        "near_timeout": near_timeout,
        "status_rows": [[code if code is not None else "(none)", cakelib.fmt_int(count)]
                        for code, count in statuses.most_common()],
    }


def judge_section(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    facts = _judge_rows(data, start, end)
    attempts = len(facts["attempts"])
    timeouts = facts["terminal_totals"].get("timeout", 0)
    non_verdict = attempts - facts["terminal_totals"].get("verdict", 0)
    ratio_rows = []
    if facts["ratios"]:
        ratio_rows.append([
            f"{100 * _p50(facts['ratios']):.0f}%",
            f"{100 * _p90(facts['ratios']):.0f}%",
            f"{100 * max(facts['ratios']):.0f}%",
            cakelib.fmt_int(len(facts["ratios"])),
            cakelib.fmt_int(facts["near_timeout"]),
            _rate_cell(facts["near_timeout"], attempts),
        ])
    return _section(
        "judge", "Historical judge reliability",
        "Legacy judge_attempt telemetry only: terminal classes, phase latency, token usage, retries, deadline utilization, and status codes.",
        cards=[
            {"label": "Judge attempts", "value": cakelib.fmt_int(attempts),
             "detail": f"timeouts {_rate_cell(timeouts, attempts)}"},
            {"label": "Non-verdict", "value": _rate_cell(non_verdict, attempts),
             "detail": "failed, timed out, or malformed"},
            {"label": "Near timeout", "value": cakelib.fmt_int(facts["near_timeout"]),
             "detail": "reached configured timeout"},
        ],
        tables=[
            _table("Per model", ["model", "attempts", *judge.TERMINAL_CLASSES,
                                 "p50", "p90", "p99", "max"],
                   facts["model_rows"], numeric=list(range(1, len(judge.TERMINAL_CLASSES) + 6)),
                   note="Every terminal class is shown, including zero counts."),
            _table("Phase latency by model (p50 / p90)", ["model", *judge.PHASES],
                   facts["phase_rows"]),
            _table("Token usage by model", ["model", "attempts w/ usage", "input", "cached",
                                            "output", "reasoning", "total"],
                   facts["usage_rows"], numeric=[1, 2, 3, 4, 5, 6]),
            _table("Retry reasons", ["reason", "count"], facts["retry_rows"], numeric=[1]),
            _table("Retry delay", ["p50", "max"], [[
                _duration(_p50(facts["delays"])), _duration(max(facts["delays"]))
            ]] if facts["delays"] else [], numeric=[0, 1]),
            _table("Deadline utilization and near-timeout attempts",
                   ["utilization p50", "p90", "max", "n", "near timeout",
                    "near-timeout share"], ratio_rows, numeric=[0, 1, 2, 3, 4, 5],
                   note="Utilization is total_ms / effective_deadline_ms. The last column is near_timeout / all judge attempts."),
            _table("Status codes", ["status", "attempts"], facts["status_rows"], numeric=[1]),
        ],
        notes=[
            "Cake no longer runs an in-process judge. This section remains for older sidecars; current gate decisions are in the policy-gates section.",
            "Judge time is cumulative attempt time and can overlap other activity.",
        ],
    )


# ---------------------------------------------------------------------------
# Time breakdown
# ---------------------------------------------------------------------------

def _timeline_rows(data: cakelib.Dataset, start: datetime, end: datetime) -> list[list]:
    rows = []
    for inv in data.invocations:
        if not inv.summary or not _in(inv.summary, inv.mtime, start, end):
            continue
        result = time_breakdown.invocation_timeline(inv)
        if result is None:
            rows.append([inv.session_id, inv.invocation_id, inv.model, inv.build_identity,
                         _duration(inv.summary.get("duration_ms")), *["-"] * 8, "precise timeline unavailable"])
            continue
        totals, missing = result
        missing_text = ", ".join(f"{count} {name}" for name, count in sorted(missing.items())) or "-"
        rows.append([inv.session_id, inv.invocation_id, inv.model, inv.build_identity,
                     _duration(inv.summary.get("duration_ms")),
                     *[_duration(totals.get(category, 0)) for category in time_breakdown.TIMELINE_CATEGORIES],
                     missing_text])
    return rows


def time_section(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    window_invocations = [inv for inv in data.invocations if inv.active_between(start, end)]
    complete = [inv for inv in window_invocations
                if inv.summary and _in(inv.summary, inv.mtime, start, end)]
    tel_calls = [call for inv in data.invocations for call in inv.tool_calls
                 if _in(call, inv.mtime, start, end)]
    window_attempts = _window_attempts(data, start, end)
    window_retries = _window_retries(data, start, end)
    wall = sum(inv.summary.get("duration_ms") or 0 for inv in complete)
    api = sum(attempt.get("total_ms", 0) for _, attempt in window_attempts)
    parse = sum(attempt.get("parse_ms", 0) for _, attempt in window_attempts)
    tools_time = sum(call.get("duration_ms", 0) for call in tel_calls)
    retry_wait = sum(retry.get("delay_ms", 0) for _, retry in window_retries)

    tool_rows = []
    for tool, calls in sorted(cakelib.group_by(tel_calls, lambda call: call.get("name", "?")).items(),
                              key=lambda item: -sum(call.get("duration_ms", 0) for call in item[1])):
        duration = sum(call.get("duration_ms", 0) for call in calls)
        tool_rows.append([tool, cakelib.fmt_int(len(calls)), _duration(duration),
                          _rate_cell(duration, tools_time)])

    api_rows = []
    for model, invocations in sorted(cakelib.group_by(window_invocations, lambda inv: inv.model).items(),
                                     key=lambda item: -len(item[1])):
        model_attempts = [(inv, attempt) for inv in invocations for attempt in inv.attempts
                          if _in(attempt, inv.mtime, start, end)]
        model_api = sum(attempt.get("total_ms", 0) for _, attempt in model_attempts)
        api_rows.append([model, cakelib.fmt_int(len(model_attempts)), _duration(model_api),
                         _rate_cell(model_api, api)])

    pace = [inv.summary["duration_ms"] / inv.summary["turn_count"]
            for inv in complete if inv.summary.get("turn_count")]
    gaps = []
    for session in data.sessions:
        durations = {record.get("task_id"): record.get("duration_ms")
                     for record in session.by_type("task_complete")}
        starts = [(cakelib.parse_ts(record.get("timestamp")), record.get("task_id"))
                  for record in session.by_type("task_start")]
        starts = [(moment, task_id) for moment, task_id in starts
                  if moment is not None and moment >= start]
        for (moment, task_id), (next_moment, _) in zip(starts, starts[1:]):
            duration = durations.get(task_id)
            if duration is not None:
                gap = (next_moment - moment).total_seconds() * 1000 - duration
                if gap >= 0:
                    gaps.append(gap)

    hook_time = sum(record.get("duration_ms") or 0
                    for session in data.sessions
                    for record in session.records_in_window(start, "hook_event", end=end))
    slow_tool_rows = []
    for call in sorted(tel_calls, key=lambda call: -(call.get("duration_ms") or 0)):
        slow_tool_rows.append([call.get("name", "?"), _duration(call.get("duration_ms")),
                               cakelib.fmt_bytes(call.get("output_bytes") or 0),
                               call.get("session_id", "?"), call.get("invocation_id", "?"),
                               cakelib.fmt_utc(_record_ts(call, None)) if _record_ts(call, None) else "-"])
    slow_api_rows = []
    for inv, attempt in sorted(_window_attempts(data, start, end),
                               key=lambda item: -(item[1].get("total_ms") or 0)):
        moment = _record_ts(attempt, inv.mtime)
        slow_api_rows.append([inv.model, _duration(attempt.get("total_ms")),
                              cakelib.fmt_int((attempt.get("usage") or {}).get("input_tokens") or 0),
                              inv.session_id, inv.invocation_id,
                              moment and cakelib.fmt_utc(moment) or "-"])

    return _section(
        "time", "Time breakdown",
        "Exclusive wall-time attribution per invocation where spans exist, cumulative work totals, turn pacing, think time, and every slow call.",
        cards=[
            {"label": "Complete invocation wall time", "value": _duration(wall),
             "detail": f"{cakelib.fmt_int(len(complete))} complete invocations"},
            {"label": "Recorded API time", "value": _duration(api),
             "detail": "cumulative, may overlap"},
            {"label": "Recorded tool work", "value": _duration(tools_time),
             "detail": "cumulative, may overlap"},
            {"label": "Hook time", "value": _duration(hook_time),
             "detail": "overlaps the tool path"},
        ],
        tables=[
            _table("Per-invocation exclusive wall time", ["session", "invocation", "model",
                                                          "identity", "wall",
                                                          *time_breakdown.TIMELINE_CATEGORIES,
                                                          "missing spans"],
                   _timeline_rows(data, start, end), numeric=[4, *range(5, 5 + len(time_breakdown.TIMELINE_CATEGORIES))],
                   note="Every complete invocation is shown. Parallel activity is counted once; unobserved time includes setup and unmeasured waits."),
            _table("Aggregate recorded activity", ["activity", "duration", "interpretation"],
                   [["model API (request+parse)", _duration(api), "cumulative per attempt"],
                    ["  of which response parsing", _duration(parse), "subset of API"],
                    ["tool execution", _duration(tools_time), "cumulative, may overlap"],
                    ["scheduled retry delay", _duration(retry_wait), "actual wait not measured"]],
                   numeric=[1]),
            _table("Cumulative tool work by tool", ["tool", "calls", "cumulative time",
                                                     "share of tool work"],
                   tool_rows, numeric=[1, 2, 3]),
            _table("API time by model", ["model", "attempts", "time", "share of API time"],
                   api_rows, numeric=[1, 2, 3]),
            _table("Turn pacing", ["metric", "p50", "p90", "max", "n"],
                   [["wall per turn", _duration(_p50(pace)), _duration(_p90(pace)),
                     _duration(max(pace)) if pace else "-", cakelib.fmt_int(len(pace))]],
                   numeric=[1, 2, 3, 4]),
            _table("Think time between tasks", ["metric", "p50", "p90", "max", "n"],
                   [["gap", _duration(_p50(gaps)), _duration(_p90(gaps)),
                     _duration(max(gaps)) if gaps else "-", cakelib.fmt_int(len(gaps))]],
                   numeric=[1, 2, 3, 4]),
            _table("Every tool call (slowest first)", ["tool", "duration", "output", "session",
                                                       "invocation", "started"],
                   slow_tool_rows, numeric=[1, 2]),
            _table("Every API attempt (slowest first)", ["model", "duration", "input tokens",
                                                         "session", "invocation", "started"],
                   slow_api_rows, numeric=[1, 2]),
        ],
        notes=[
            "Aggregate durations are cumulative work and cannot be subtracted from wall time. Use the per-invocation timeline for exclusive attribution when span records exist.",
            "Bash policy-gate time is estimated within each Bash span; Bash remaining includes command setup, execution, output handling, and hooks.",
        ],
    )


# ---------------------------------------------------------------------------
# Hooks, skills, and compensations
# ---------------------------------------------------------------------------

def hooks_section(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    events = [(session, record) for session in data.sessions
              for record in session.records_in_window(start, "hook_event", end=end)]
    by_event = Counter(record.get("event", "?") for _, record in events)
    event_rows = [[name, cakelib.fmt_int(count)] for name, count in by_event.most_common()]
    decisions = Counter((record.get("decision") or "-", record.get("resolved_decision") or "-")
                        for _, record in events)
    decision_rows = [[decision, resolved, cakelib.fmt_int(count),
                      _rate_cell(count, len(events))]
                     for (decision, resolved), count in decisions.most_common()]
    denies = [(session, record) for session, record in events
              if record.get("decision") == "deny" or record.get("resolved_decision") == "deny"]
    deny_rows = [[session.id, session.model, record.get("timestamp") or "-",
                  record.get("tool_name") or "?", record.get("tool_input_summary") or "-",
                  record.get("decision") or "-", record.get("resolved_decision") or "-",
                  record.get("exit_code") if record.get("exit_code") is not None else "-",
                  _duration(record.get("duration_ms"))]
                 for session, record in denies]
    hook_errors = [record for _, record in events if record.get("decision") == "error"]
    fail_closed = [record for record in hook_errors if record.get("fail_closed")]
    exits = Counter(record.get("exit_code") for _, record in events
                    if record.get("exit_code") not in (0, None))
    exit_rows = [[code, cakelib.fmt_int(count)] for code, count in exits.most_common()]
    durations = [record["duration_ms"] for _, record in events
                 if record.get("duration_ms") is not None]
    commands = Counter((record.get("command") or "?") for _, record in events)
    command_rows = [[command, cakelib.fmt_int(count)]
                    for command, count in commands.most_common()]
    event_rows_detail = [[record.get("timestamp") or "-", session.id, session.model,
                          record.get("event", "?"), record.get("decision") or "-",
                          record.get("resolved_decision") or "-",
                          record.get("tool_name") or "-",
                          record.get("tool_input_summary") or "-",
                          record.get("exit_code") if record.get("exit_code") is not None else "-",
                          "yes" if record.get("fail_closed") else "no",
                          _duration(record.get("duration_ms")),
                          record.get("command") or "-"]
                         for session, record in events]
    with_catalog = sum(1 for session in data.sessions if _has_skill_catalog(session))
    skills = [(session, record) for session in data.sessions
              for record in session.records_in_window(start, "skill_activated", end=end)]
    skill_rows = [[session.id, session.model, record.get("timestamp") or "-",
                   record.get("name", "?"), str(record.get("path") or "-")]
                  for session, record in skills]
    skill_counts = Counter(record.get("name", "?") for _, record in skills)
    skill_count_rows = [[name, cakelib.fmt_int(count)]
                        for name, count in skill_counts.most_common()]
    sessions_using = sum(1 for session in data.sessions
                         if session.records_in_window(start, "skill_activated", end=end))
    return _section(
        "hooks", "Hooks and skills",
        "Every hook event, decision, denial, infrastructure error, exit code, command, duration, and skill activation.",
        cards=[
            {"label": "Hook events", "value": cakelib.fmt_int(len(events)),
             "detail": f"denied {cakelib.fmt_int(len(denies))}"},
            {"label": "Infrastructure errors", "value": cakelib.fmt_int(len(hook_errors)),
             "detail": f"fail-closed {cakelib.fmt_int(len(fail_closed))}"},
            {"label": "Skill activations", "value": cakelib.fmt_int(len(skills)),
             "detail": f"{cakelib.fmt_int(with_catalog)} sessions had a skill catalog"},
        ],
        tables=[
            _table("Events", ["event", "count"], event_rows, numeric=[1]),
            _table("Decisions", ["decision", "resolved", "count", "share"],
                   decision_rows, numeric=[2, 3]),
            _table("Denied tool calls", ["session", "model", "timestamp", "tool", "input",
                                         "decision", "resolved", "exit", "duration"],
                   deny_rows, numeric=[7, 8],
                   note="Hook input summaries and commands are shown as recorded. This page is owner-only."),
            _table("Infrastructure errors and nonzero exits", ["exit code", "count"],
                   exit_rows, numeric=[1]),
            _table("Hook duration", ["metric", "p50", "p90", "max", "n"],
                   [["duration", _duration(_p50(durations)), _duration(_p90(durations)),
                     _duration(max(durations)) if durations else "-", cakelib.fmt_int(len(durations))]],
                   numeric=[1, 2, 3, 4]),
            _table("Hook commands", ["command", "count"], command_rows, numeric=[1]),
            _table("Every hook event", ["timestamp", "session", "model", "event", "decision",
                                        "resolved", "tool", "input", "exit", "fail closed",
                                        "duration", "command"],
                   event_rows_detail, numeric=[8, 10],
                   note="All hook events in the window, not a top-N sample."),
            _table("Skills available", ["metric", "value"],
                   [["sessions with a skill catalog", cakelib.fmt_int(with_catalog)],
                    ["sessions activating at least one skill", cakelib.fmt_int(sessions_using)]],
                   numeric=[1]),
            _table("Skill activations", ["skill", "activations"], skill_count_rows, numeric=[1]),
            _table("Every skill activation", ["session", "model", "timestamp", "skill", "path"],
                   skill_rows),
        ],
        notes=[
            "A skill activates at most once per session. A resumed invocation reuses the activation without a new record.",
            "A skill loaded through Bash is deliberately not recorded; the transcript only records the first Read of a known SKILL.md path.",
        ],
    )


def _has_skill_catalog(session: cakelib.Session) -> bool:
    for record in session.records:
        content = record.get("content")
        if isinstance(content, str) and "<location>" in content and "SKILL.md" in content:
            return True
    return False


def compensations_section(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    window_data = _window_dataset(data, start, end)
    by_model, by_kind_detail, judge_latencies, overflow_by_model, judge_overridden = \
        compensations.count_events(window_data)
    all_models = sorted({inv.model for inv in window_data.invocations})
    rows = []
    for model in all_models:
        counts = by_model.get(model, Counter())
        overflow = overflow_by_model.get(model, 0)
        kind_total = sum(count for kind, count in counts.items()
                         if kind != compensations.RETRY_DERIVED_KIND)
        rows.append([model, *[cakelib.fmt_int(counts.get(kind, 0))
                              for kind in compensations.KINDS],
                     cakelib.fmt_int(overflow), cakelib.fmt_int(kind_total + overflow)])
    detail_rows = [[kind, detail, cakelib.fmt_int(count)]
                   for (kind, detail), count in by_kind_detail.most_common()]
    override_rows = [[model, cakelib.fmt_int(count)]
                     for model, count in judge_overridden.most_common()]
    identity_counts = Counter()
    for inv in window_data.invocations:
        for event in inv.compensations:
            if _in(event, inv.mtime, start, end):
                identity_counts[(inv.build_identity, event.get("kind", "?"))] += 1
    identity_rows = [[identity, kind, cakelib.fmt_int(count)]
                     for (identity, kind), count in sorted(identity_counts.items())]
    return _section(
        "compensations", "Model compensations",
        "Every compensation counter per model, including flatlined zeros that mark deletion candidates.",
        cards=[
            {"label": "Compensation events",
             "value": cakelib.fmt_int(sum(sum(counts.values()) for counts in by_model.values())),
             "detail": "recorded across all kinds"},
            {"label": "Judge block overrides",
             "value": cakelib.fmt_int(sum(judge_overridden.values())),
             "detail": "allowlist overrode a block"},
            {"label": "Judge verdict latency", "value": _duration(_p50(judge_latencies)),
             "detail": f"n={cakelib.fmt_int(len(judge_latencies))}"},
        ],
        tables=[
            _table("Per model", ["model", *compensations.KINDS, "context-overflow retries", "total"],
                   rows, numeric=list(range(1, len(compensations.KINDS) + 3)),
                   note="A zero row is a deletion-candidate signal, not missing data. Every configured model is shown."),
            _table("By compensation and detail", ["kind", "detail", "count"],
                   detail_rows, numeric=[2]),
            _table("Judge verdict latency", ["metric", "p50", "p90", "max", "n"],
                   [["verdict latency", _duration(_p50(judge_latencies)),
                     _duration(_p90(judge_latencies)),
                     _duration(max(judge_latencies)) if judge_latencies else "-",
                     cakelib.fmt_int(len(judge_latencies))]], numeric=[1, 2, 3, 4]),
            _table("Judge block overridden by allowlist", ["model", "count"],
                   override_rows, numeric=[1]),
            _table("Compensations by build identity", ["identity", "kind", "count"],
                   identity_rows, numeric=[2]),
        ],
        notes=[
            "Each counter maps to a compensation cake carries for a model weakness. Review a zero counter before deleting the compensation.",
            "The context-overflow column is counted from retry records so legacy sidecars count once.",
        ],
    )


# ---------------------------------------------------------------------------
# Session and invocation inventory
# ---------------------------------------------------------------------------

def _session_created(session: cakelib.Session) -> datetime | None:
    for record in session.records:
        if record.get("type") in ("session_meta", "session_start", "init"):
            return _record_ts(record, session.mtime)
    return session.mtime


def _session_rows(data: cakelib.Dataset, start: datetime, end: datetime) -> list[list]:
    rows = []
    for session in sorted(data.sessions, key=lambda item: item.last_activity, reverse=True):
        tasks = session.tasks_in_window(start, end)
        task_count = len(tasks)
        success = sum(task.get("subtype") == "success" for task in tasks)
        invocations = [inv for inv in data.invocations if inv.session_id == session.id]
        usage = cakelib.usage_totals([
            attempt.get("usage") for inv in invocations
            for attempt in inv.attempts
            if attempt.get("usage") and _in(attempt, inv.mtime, start, end)
        ])
        created = _session_created(session)
        rows.append([
            session.id,
            cakelib.fmt_utc(created) if created else "-",
            cakelib.fmt_utc(session.last_activity),
            session.model,
            session.working_directory,
            session.cake_version,
            session.build_identity,
            cakelib.fmt_bytes(session.size),
            cakelib.fmt_int(len(session.records)),
            cakelib.fmt_int(len([record for record in session.by_type("message")
                                 if record.get("role") == "user"])),
            cakelib.fmt_int(len([record for record in session.by_type("message")
                                 if record.get("role") == "assistant"])),
            cakelib.fmt_int(len(session.tool_calls)),
            cakelib.fmt_int(task_count),
            "yes" if session.inflight_in_window(start) else "no",
            cakelib.fmt_int(len(invocations)),
            cakelib.fmt_int(usage["total"]),
            _rate_cell(success, task_count),
        ])
    return rows


def _invocation_rows(data: cakelib.Dataset, start: datetime, end: datetime) -> list[list]:
    rows = []
    for inv in sorted(data.invocations, key=lambda item: item.last_activity or item.mtime or start,
                      reverse=True):
        if not inv.active_between(start, end):
            continue
        init = inv.init or {}
        summary = inv.summary or {}
        usage = summary.get("usage") or {}
        rows.append([
            inv.session_id, inv.invocation_id,
            init and cakelib.fmt_utc(_record_ts(init, inv.mtime)) or "-",
            inv.last_activity and cakelib.fmt_utc(inv.last_activity) or "-",
            inv.model, inv.working_directory, inv.build_identity,
            init.get("mode", "-"), init.get("api_type", "-"), init.get("output_format", "-"),
            cakelib.fmt_int(len(inv.attempts)),
            cakelib.fmt_int(len(inv.tool_calls)),
            cakelib.fmt_int(len(inv.retries)),
            cakelib.fmt_int(len(inv.gate_decisions)),
            cakelib.fmt_int(len(inv.judge_attempts)),
            "yes" if summary.get("success") else ("no" if summary else "no summary"),
            _duration(summary.get("duration_ms")),
            cakelib.fmt_int(summary.get("turn_count") or 0),
            cakelib.fmt_int(usage.get("total_tokens") or 0),
        ])
    return rows


def _configuration_tables(data: cakelib.Dataset, start: datetime, end: datetime) -> list[dict]:
    """Inventory tables that mirror the terminal overview section."""
    model_sessions = Counter(session.model for session in data.sessions)
    model_invocations = Counter(inv.model for inv in data.invocations)
    model_tasks = Counter()
    model_success = Counter()
    for session in data.sessions:
        for task in session.tasks_in_window(start, end):
            model_tasks[session.model] += 1
            model_success[session.model] += task.get("subtype") == "success"
    model_rows = [
        [model, cakelib.fmt_int(model_sessions.get(model, 0)),
         _rate_cell(model_sessions.get(model, 0), len(data.sessions)),
         cakelib.fmt_int(model_invocations.get(model, 0)),
         cakelib.fmt_int(model_tasks.get(model, 0)),
         _rate_cell(model_success.get(model, 0), model_tasks.get(model, 0))]
        for model in sorted(set(model_sessions) | set(model_invocations))
    ]

    version_sessions = Counter(session.cake_version for session in data.sessions)
    version_invocations = Counter(inv.cake_version or "unknown" for inv in data.invocations)
    version_tasks = Counter()
    for session in data.sessions:
        for _task in session.tasks_in_window(start, end):
            version_tasks[session.cake_version] += 1
    version_rows = [
        [version, cakelib.fmt_int(version_sessions.get(version, 0)),
         cakelib.fmt_int(version_invocations.get(version, 0)),
         cakelib.fmt_int(version_tasks.get(version, 0))]
        for version in sorted(set(version_sessions) | set(version_invocations))
    ]

    project_sessions = Counter(session.working_directory for session in data.sessions)
    project_invocations = Counter(inv.working_directory for inv in data.invocations)
    project_rows = [
        [project, cakelib.fmt_int(project_sessions.get(project, 0)),
         cakelib.fmt_int(project_invocations.get(project, 0))]
        for project in sorted(set(project_sessions) | set(project_invocations))
    ]

    modes = Counter((inv.init or {}).get("mode") or "unknown" for inv in data.invocations)
    mode_rows = [[mode, cakelib.fmt_int(count), _rate_cell(count, len(data.invocations))]
                 for mode, count in modes.most_common()]
    api_types = Counter(((inv.init or {}).get("api_type") or "?",
                         (inv.init or {}).get("output_format") or "?")
                        for inv in data.invocations)
    api_rows = [[api_type, output_format, cakelib.fmt_int(count)]
                for (api_type, output_format), count in api_types.most_common()]

    settings_fields = (
        ("reasoning_effort", "Reasoning effort"),
        ("max_output_tokens", "Max output tokens"),
        ("reasoning_max_tokens", "Reasoning max tokens"),
    )
    setting_tables = []
    for field, label in settings_fields:
        values = Counter()
        for inv in data.invocations:
            settings = (inv.init or {}).get("settings") or {}
            value = settings.get(field)
            values[value if value is not None else "unknown"] += 1
        setting_tables.append(_table(
            label,
            [label.lower(), "invocations"],
            [[value, cakelib.fmt_int(count)] for value, count in values.most_common()],
            numeric=[1],
        ))

    tools_present = Counter(
        tool for inv in data.invocations
        for tool in dict.fromkeys(((inv.init or {}).get("tools") or []))
    )
    tools_rows = [[tool, cakelib.fmt_int(count)]
                  for tool, count in tools_present.most_common()]

    shape_rows = [
        ["file size", cakelib.fmt_bytes(median(session.size for session in data.sessions)) if data.sessions else "-"],
        ["records", cakelib.fmt_int(median(len(session.records) for session in data.sessions)) if data.sessions else "-"],
        ["user messages", cakelib.fmt_int(median(
            len([record for record in session.by_type("message") if record.get("role") == "user"])
            for session in data.sessions)) if data.sessions else "-"],
        ["assistant messages", cakelib.fmt_int(median(
            len([record for record in session.by_type("message") if record.get("role") == "assistant"])
            for session in data.sessions)) if data.sessions else "-"],
        ["tool calls", cakelib.fmt_int(median(len(session.tool_calls) for session in data.sessions)) if data.sessions else "-"],
        ["tasks", cakelib.fmt_int(median(len(session.tasks_in_window(start, end)) for session in data.sessions)) if data.sessions else "-"],
    ]

    return [
        _table("Models", ["model", "sessions", "share", "invocations", "tasks", "completion"],
               model_rows, numeric=[1, 2, 3, 4, 5]),
        _table("Cake versions", ["cake version", "sessions", "invocations", "tasks"],
               version_rows, numeric=[1, 2, 3]),
        _table("Projects", ["project", "sessions", "invocations"],
               project_rows, numeric=[1, 2]),
        _table("Run modes", ["mode", "invocations", "share"], mode_rows, numeric=[1, 2]),
        _table("API type / output format", ["api type", "output format", "invocations"],
               api_rows, numeric=[2]),
        *setting_tables,
        _table("Tools present", ["tool", "invocations"], tools_rows, numeric=[1]),
        _table("Per-session shape (median)", ["metric", "median"], shape_rows),
    ]


def sessions_section(data: cakelib.Dataset, start: datetime, end: datetime) -> dict:
    return _section(
        "sessions", "Sessions and invocations",
        "Every session transcript and every invocation sidecar in the window, with counts, identity, and outcome. Raw prompts and tool output stay in the session files.",
        tables=[
            *_configuration_tables(data, start, end),
            _table("Sessions", ["session", "created", "last activity", "model", "project",
                                "cake version", "identity", "size", "records", "user msgs",
                                "assistant msgs", "tool calls", "tasks", "incomplete",
                                "invocations", "tokens", "completion"],
                   _session_rows(data, start, end), numeric=[7, 8, 9, 10, 11, 12, 14, 15, 16],
                   note="All sessions in the window. `completion` is over completed tasks in the window."),
            _table("Invocations", ["session", "invocation", "started", "last activity", "model",
                                   "project", "identity", "mode", "api type", "output format",
                                   "attempts", "tools", "retries", "gate decisions",
                                   "judge attempts", "summary", "duration", "turns", "tokens"],
                   _invocation_rows(data, start, end), numeric=[10, 11, 12, 13, 14, 16, 17, 18],
                   note="Every invocation active in the window. A missing session_summary is shown as `no summary`."),
        ],
        notes=[
            "Session IDs and project paths are local identifiers. This page is private and owner-only.",
            "Per-session raw record pages are deferred to a separate detail page; this aggregate page stays metadata-only.",
        ],
    )


# ---------------------------------------------------------------------------
# Data dictionary
# ---------------------------------------------------------------------------

DEFINITIONS = (
    ("Completion", "Share of completed tasks whose recorded subtype is `success`.", "transcript task_complete", "Operational success, not correctness."),
    ("Abnormal termination", "Invocations that started but wrote no session_summary.", "telemetry init/summary", "A live run can appear here until it finishes."),
    ("Tool failure rate", "Failed paired tool calls over all paired tool calls.", "transcript function_call_output", "Synthetic `not executed:` outputs are not failures."),
    ("Edit no-match rate", "Edit failures classified `no-match (old_text not found)` over all Edit calls.", "transcript", "The category is defined by cakelib.classify_tool_error."),
    ("API failure rate", "Attempts with an error or a status other than 200 over all attempts.", "telemetry api_attempt", "A missing status is not a failure by itself."),
    ("API latency p95", "Nearest-rank 95th percentile of request_ms.", "telemetry api_attempt", "Sample count is shown."),
    ("Cache-hit rate", "Cached input tokens over input tokens from requests that reported cached-token details.", "telemetry api_attempt usage", "The denominator excludes requests that did not report cache details."),
    ("Gate failure rate", "Failures and disabled outcomes over decisions excluding bypass.", "telemetry gate_decision", "A startup failure is reported separately."),
    ("Gate block rate", "Block outcomes over allow+warn+block+abstain decisions.", "telemetry gate_decision", "Bypass and failures are excluded from the denominator."),
    ("Judge timeout rate", "Legacy judge attempts whose terminal class is `timeout`.", "telemetry judge_attempt", "Current gates do not use judge_attempt."),
    ("Judge failure rate", "Legacy judge attempts whose terminal class is not `verdict`.", "telemetry judge_attempt", "Includes timeout, transport, HTTP, parse, refusal, and malformed verdict."),
    ("Judge block rate", "Block verdicts over allow+warn+block verdicts.", "telemetry compensation", "Only judge_verdict events carry a verdict."),
    ("Tokens per task", "(Input + output tokens) / completed tasks.", "telemetry attempts and transcript tasks", "Telemetry covers only invocations with sidecars."),
    ("Task p50/p90", "Nearest-rank percentile of task duration_ms.", "transcript task_complete", "Sample count is shown."),
    ("Input tokens/request", "Provider-reported input tokens per attempt.", "telemetry api_attempt usage", "Context growth, not a cost estimate."),
    ("History items/request", "History items recorded on an API attempt.", "telemetry api_attempt", "A context-size proxy."),
    ("Tool duration p50/p90", "Nearest-rank percentile of telemetry tool_call duration_ms.", "telemetry tool_call", "Transcript calls have no duration."),
    ("Output bytes", "Provider-visible tool output size recorded on telemetry.", "telemetry tool_call", "Not the same as transcript output text."),
    ("Retry recovery", "A failed Edit/Write retried on the same path later in the same session, and eventually succeeded.", "transcript", "Attributed to the failed call's day."),
    ("Permission denial", "A task_complete permission_denials label, attributed to its gate by prefix.", "transcript task_complete", "The source name is the only label-derived value reported."),
    ("Compensation", "A recorded model-specific rescue event.", "telemetry compensation", "A zero counter is a deletion-candidate signal."),
    ("Cache break", "A turn whose missed prompt tokens exceed the 1,024-token noise floor.", "telemetry api_attempt usage", "The scan never reads prompt text."),
    ("Exclusive wall time", "Interval attribution of one invocation across provider, retry, tool, safety, parallel, and unobserved categories.", "telemetry spans + session_summary", "Requires span records; historical invocations may show missing spans."),
    ("Cumulative tool work", "Sum of tool_call duration_ms; work may overlap.", "telemetry tool_call", "Cannot be subtracted from wall time."),
    ("Build identity", "Full build commit plus dirty state, falling back to cake_version.", "session_meta / telemetry_init", "Primary key for change attribution."),
)


def definitions_section() -> dict:
    rows = [[metric, definition, source, caveat]
            for metric, definition, source, caveat in DEFINITIONS]
    return _section(
        "definitions", "Data dictionary",
        "Every headline metric and every derived table column used on this page, with its source and caveat.",
        tables=[_table("Definitions", ["metric", "definition", "source", "caveat"], rows)],
        notes=["Definitions are visible on the page, not hidden behind a tooltip."],
    )


# ---------------------------------------------------------------------------
# Assembly
# ---------------------------------------------------------------------------

def build_report(data: cakelib.Dataset, start: datetime, end: datetime,
                 now: datetime) -> dict:
    """Build every dashboard section for one half-open window."""
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
    return {
        "schema_version": 2,
        "measured": cakelib.fmt_utc(now),
        "window": {"start": cakelib.fmt_utc(start), "end": cakelib.fmt_utc(end)},
        "headline": headline,
        "attribution": change_attribution(data, start, end),
        "sections": sections,
    }
