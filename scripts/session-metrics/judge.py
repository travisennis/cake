#!/usr/bin/env python3
"""LLM-judge operational health from per-attempt telemetry.

The Bash command-safety judge is cake's only non-sandbox command gate. Its
health in production (field) is measured here from `judge_attempt` telemetry
records, complementing the fixed-corpus SLO benchmark (`just judge-bench`):
latency percentiles and phase timing, terminal-class distribution (timeout,
transport, http_error, response_parse, malformed_verdict, refusal), retry
behavior, token cost, status codes, and near-timeout attempts, per model.

Source: telemetry `judge_attempt` records.

This report owns judge latency, cost, and reliability. `compensations.py`
keeps the `judge_verdict` / `judge_fail_closed` / `judge_bypass` counters,
which measure how often cake compensates for a model weakness; the two
sections do not duplicate the latency table.
"""

from collections import Counter

import cakelib
from cakelib import fmt_int, fmt_ms, fmt_pct, percentile, print_header, print_table

TERMINAL_CLASSES = [
    "verdict",
    "timeout",
    "transport",
    "http_error",
    "response_parse",
    "malformed_verdict",
    "refusal",
]

PHASES = ["request_build_ms", "request_ms", "response_parse_ms", "verdict_parse_ms"]


def aggregate(data: cakelib.Dataset) -> dict[str, dict]:
    """Collect per-model judge-attempt statistics for testing.

    Returns a dict model -> stats dict, where stats holds raw aggregates:
    - attempts: full attempt records
    - terminal: Counter of terminal_class -> count
    - latency: list of total_ms
    - phases: {phase_name: list of ms values}
    - retry_attempts / retry_reasons / retry_delays
    - deadline_ratio: list of total_ms / effective_deadline_ms (retry-era only)
    - near_timeout: attempts whose total_ms reached configured_timeout_ms
    - configured_timeout: last configured_timeout_ms observed
    - usage: list of usage dicts
    - status: Counter of status_code -> count

    Metadata only: nothing here carries a command, reason, cwd, prompt, or
    response text.
    """
    by_model: dict[str, dict] = {}
    for inv in data.invocations:
        attempts = inv.judge_attempts
        if not attempts:
            continue
        stats = by_model.setdefault(
            inv.model,
            {
                "attempts": [],
                "terminal": Counter(),
                "latency": [],
                "phases": {p: [] for p in PHASES},
                "retry_attempts": [],
                "retry_reasons": Counter(),
                "retry_delays": [],
                "deadline_ratio": [],
                "near_timeout": 0,
                "configured_timeout": 0,
                "usage": [],
                "status": Counter(),
            },
        )
        for a in attempts:
            stats["terminal"][a.get("terminal_class", "?")] += 1
            total = a.get("total_ms") or 0
            stats["attempts"].append(a)
            stats["latency"].append(total)
            for phase in PHASES:
                value = a.get(phase)
                if value is not None:
                    stats["phases"][phase].append(value)
            if a.get("retry_ordinal", 0) > 0:
                stats["retry_attempts"].append(a)
                stats["retry_reasons"][a.get("retry_reason", "?")] += 1
                delay = a.get("retry_delay_ms")
                if delay is not None:
                    stats["retry_delays"].append(delay)
            deadline = a.get("effective_deadline_ms")
            if deadline:
                stats["deadline_ratio"].append(total / deadline)
            configured = a.get("configured_timeout_ms")
            if configured is not None:
                stats["configured_timeout"] = configured
                if total >= configured:
                    stats["near_timeout"] += 1
            if a.get("usage"):
                stats["usage"].append(a["usage"])
            stats["status"][a.get("status_code")] += 1
    return by_model


def _fmt_ratio(values: list[float], pct: float) -> str:
    return f"{percentile(values, pct) * 100:.0f}%" if values else "-"


def run(data: cakelib.Dataset) -> None:
    print_header("JUDGE RELIABILITY")
    print(cakelib.describe_window(data))

    by_model = aggregate(data)
    all_attempts = [a for stats in by_model.values() for a in stats["attempts"]]
    if not all_attempts:
        print("\nNo telemetry judge_attempt records in window.")
        return

    terminal_totals = Counter()
    for stats in by_model.values():
        terminal_totals.update(stats["terminal"])
    attempts = len(all_attempts)
    timeouts = terminal_totals.get("timeout", 0)
    non_verdict = attempts - terminal_totals.get("verdict", 0)
    print(
        f"\nAttempts: {fmt_int(attempts)} | timeouts: {fmt_int(timeouts)} "
        f"({fmt_pct(timeouts, attempts)}) | non-verdict: {fmt_int(non_verdict)} "
        f"({fmt_pct(non_verdict, attempts)})"
    )

    print("\nPer model (judge attempts, latency, terminal class):")
    rows = []
    for model in sorted(by_model):
        stats = by_model[model]
        lat = stats["latency"]
        rows.append(
            [model, fmt_int(len(lat))]
            + [fmt_int(stats["terminal"].get(cls, 0)) for cls in TERMINAL_CLASSES]
            + [
                fmt_ms(percentile(lat, 50)),
                fmt_ms(percentile(lat, 90)),
                fmt_ms(percentile(lat, 99)),
                fmt_ms(max(lat or [0])),
            ]
        )
    print_table(
        ["model", "attempts"] + TERMINAL_CLASSES + ["p50", "p90", "p99", "max"],
        rows,
    )

    print("\nJudge phase latency by model (p50 / p90):")
    phase_rows = []
    for model in sorted(by_model):
        stats = by_model[model]
        cells = [model]
        for phase in PHASES:
            values = stats["phases"][phase]
            cells.append(
                f"{fmt_ms(percentile(values, 50))}/{fmt_ms(percentile(values, 90))}"
            )
        phase_rows.append(cells)
    print_table(["model"] + [p[:-3] for p in PHASES], phase_rows)

    print("\nJudge token usage by model (attempts with provider usage):")
    usage_rows = []
    for model in sorted(by_model):
        stats = by_model[model]
        usage = cakelib.usage_totals(stats["usage"])
        usage_rows.append(
            [model, fmt_int(len(stats["usage"]))]
            + [fmt_int(usage[k]) for k in ("input", "cached", "output", "reasoning", "total")]
        )
    print_table(
        ["model", "attempts w/ usage", "input", "cached", "output", "reasoning", "total"],
        usage_rows,
    )

    retried = [a for stats in by_model.values() for a in stats["retry_attempts"]]
    if retried:
        print(
            f"\nRetried attempts: {fmt_int(len(retried))} "
            f"({fmt_pct(len(retried), attempts)} of attempts)"
        )
        reasons = Counter(a.get("retry_reason", "?") for a in retried)
        print_table(["retry reason", "count"], [[r, fmt_int(n)] for r, n in reasons.most_common()])
        delays = [a.get("retry_delay_ms", 0) for a in retried]
        print_table(
            ["retry delay p50", "retry delay max"],
            [[fmt_ms(percentile(delays, 50)), fmt_ms(max(delays))]],
        )
    else:
        print("\nRetried attempts: 0")

    ratios = [r for stats in by_model.values() for r in stats["deadline_ratio"]]
    near_timeout = sum(stats["near_timeout"] for stats in by_model.values())
    if ratios or near_timeout:
        print()
        if ratios:
            print_table(
                ["deadline utilization p50", "p90", "max"],
                [[_fmt_ratio(ratios, 50), _fmt_ratio(ratios, 90), _fmt_ratio(ratios, 100)]],
            )
        if near_timeout:
            print(f"Attempts that reached their configured timeout: {fmt_int(near_timeout)}")
    else:
        print()

    print("\nJudge status codes:")
    status_totals = Counter()
    for stats in by_model.values():
        status_totals.update(stats["status"])
    print_table(
        ["status", "attempts"],
        [[c if c is not None else "(none)", fmt_int(n)] for c, n in status_totals.most_common()],
    )


def main() -> None:
    ns = cakelib.build_arg_parser(__doc__).parse_args()
    run(cakelib.load(ns))


if __name__ == "__main__":
    main()