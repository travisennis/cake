#!/usr/bin/env python3
"""Token usage: totals, cache efficiency, per model/project/day, context growth.

Primary source is telemetry api_attempt records (per-request usage). Session
transcripts' task_complete/result usage is reported as a cross-check because
telemetry only exists for sessions run since sidecars were introduced.
"""

from collections import defaultdict

import cakelib
from cakelib import fmt_int, fmt_pct, percentile, print_header, print_table, usage_totals


def _usage_rows(grouped: dict[str, dict[str, int]]) -> list[list[str]]:
    rows = []
    order = sorted(grouped.items(), key=lambda kv: -kv[1]["total"])
    for key, t in order:
        rows.append([
            key, fmt_int(t["input"]), fmt_int(t["cached"]), fmt_pct(t["cached"], t["input"]),
            fmt_int(t["output"]), fmt_int(t["reasoning"]), fmt_int(t["total"]),
        ])
    return rows


USAGE_HEADERS = ["", "input", "cached", "cache%", "output", "reasoning", "total"]


def run(data: cakelib.Dataset) -> None:
    print_header("TOKEN USAGE")
    print(cakelib.describe_window(data))

    attempts = [a for inv in data.invocations for a in inv.attempts if a.get("usage")]
    if attempts:
        print("\nTotals (telemetry api_attempt):")
        totals = usage_totals([a["usage"] for a in attempts])
        print_table(USAGE_HEADERS, _usage_rows({"all models": totals}))

        print("\nBy model:")
        by_model: dict[str, list[dict]] = defaultdict(list)
        for inv in data.invocations:
            for a in inv.attempts:
                if a.get("usage"):
                    by_model[inv.model].append(a["usage"])
        print_table(USAGE_HEADERS, _usage_rows({m: usage_totals(u) for m, u in by_model.items()}))

        print("\nBy project:")
        by_project: dict[str, list[dict]] = defaultdict(list)
        for inv in data.invocations:
            for a in inv.attempts:
                if a.get("usage"):
                    by_project[inv.working_directory].append(a["usage"])
        print_table(USAGE_HEADERS, _usage_rows({p: usage_totals(u) for p, u in by_project.items()}))

        print("\nBy day:")
        by_day: dict[str, list[dict]] = defaultdict(list)
        for a in attempts:
            ts = cakelib.parse_ts(a.get("timestamp"))
            if ts:
                by_day[ts.date().isoformat()].append(a["usage"])
        print_table(USAGE_HEADERS, [_usage_rows({d: usage_totals(by_day[d])})[0] for d in sorted(by_day)])

        print("\nPer-invocation totals (session_summary):")
        inv_totals = [inv.summary["usage"]["total_tokens"] for inv in data.invocations
                      if inv.summary and inv.summary.get("usage")]
        if inv_totals:
            print_table(["p50", "p90", "max"], [[
                fmt_int(percentile(inv_totals, 50)),
                fmt_int(percentile(inv_totals, 90)),
                fmt_int(max(inv_totals)),
            ]])

        print("\nContext growth (per api_attempt):")
        input_sizes = [a["usage"]["input_tokens"] for a in attempts]
        history = [a.get("history_items", 0) for a in attempts]
        print_table(
            ["metric", "p50", "p90", "max"],
            [
                ["input tokens/request", fmt_int(percentile(input_sizes, 50)),
                 fmt_int(percentile(input_sizes, 90)), fmt_int(max(input_sizes))],
                ["history items/request", fmt_int(percentile(history, 50)),
                 fmt_int(percentile(history, 90)), fmt_int(max(history))],
            ],
        )
    else:
        print("\nNo telemetry api_attempt usage in window.")

    # Cross-check from transcripts (covers sessions without sidecars).
    task_usages = []
    for s in data.sessions:
        for rec in s.tasks_in_window(data.cutoff):
            if rec.get("usage"):
                task_usages.append(rec["usage"])
    if task_usages:
        print("\nCross-check - transcript task_complete/result usage totals:")
        print_table(USAGE_HEADERS, _usage_rows({"all sessions": usage_totals(task_usages)}))


def main() -> None:
    ns = cakelib.build_arg_parser(__doc__).parse_args()
    run(cakelib.load(ns))


if __name__ == "__main__":
    main()
