#!/usr/bin/env python3
"""Tool call metrics: volume, success/failure, failure taxonomy, retry recovery,
durations and output sizes, and per-turn parallelism."""

from collections import Counter, defaultdict

import cakelib
from cakelib import (
    classify_tool_error, fmt_bytes, fmt_int, fmt_ms, fmt_pct, percentile,
    print_header, print_table,
)

FILE_TOOLS = ("Edit", "Write")


def run(data: cakelib.Dataset) -> None:
    print_header("TOOL CALLS")
    print(cakelib.describe_window(data))

    all_calls = [(s, c) for s in data.sessions for c in s.tool_calls]
    if not all_calls:
        print("\nNo tool calls in window.")
        return

    print("\nPer tool (from transcripts):")
    by_tool = cakelib.group_by(all_calls, lambda sc: sc[1].name)
    rows = []
    for tool, calls in sorted(by_tool.items(), key=lambda kv: -len(kv[1])):
        failures = [c for _, c in calls if not c.ok]
        rows.append([tool, fmt_int(len(calls)), fmt_int(len(failures)),
                     fmt_pct(len(failures), len(calls))])
    total_failures = sum(1 for _, c in all_calls if not c.ok)
    rows.append(["TOTAL", fmt_int(len(all_calls)), fmt_int(total_failures),
                 fmt_pct(total_failures, len(all_calls))])
    print_table(["tool", "calls", "failures", "failure rate"], rows)

    print("\nFailure taxonomy:")
    taxonomy = Counter()
    for _, c in all_calls:
        if not c.ok:
            taxonomy[(c.name, classify_tool_error(c.name, c.output))] += 1
    print_table(
        ["tool", "category", "count"],
        [[tool, cat, n] for (tool, cat), n in taxonomy.most_common()],
    )

    print("\nRetry recovery after failure (Edit/Write, same file, same session):")
    rows = []
    for tool in FILE_TOOLS:
        retried = recovered = abandoned = 0
        for s in data.sessions:
            calls = s.tool_calls
            for i, c in enumerate(calls):
                if c.name != tool or c.ok:
                    continue
                later = [d for d in calls[i + 1:]
                         if d.name in FILE_TOOLS and d.path == c.path and c.path]
                if later:
                    retried += 1
                    if any(d.ok for d in later):
                        recovered += 1
                else:
                    abandoned += 1
        if retried or abandoned:
            rows.append([tool, retried, recovered, fmt_pct(recovered, retried), abandoned])
    print_table(["tool", "retried", "recovered", "recovery rate", "not retried"], rows)

    tel_calls = [tc for inv in data.invocations for tc in inv.tool_calls]
    if tel_calls:
        print("\nDurations and output sizes (telemetry):")
        rows = []
        for tool, calls in sorted(cakelib.group_by(tel_calls, lambda t: t["name"]).items(),
                                  key=lambda kv: -len(kv[1])):
            durations = [c["duration_ms"] for c in calls]
            sizes = [c["output_bytes"] for c in calls]
            rows.append([
                tool, fmt_int(len(calls)),
                fmt_ms(percentile(durations, 50)), fmt_ms(percentile(durations, 90)),
                fmt_ms(max(durations)),
                fmt_bytes(percentile(sizes, 50)), fmt_bytes(max(sizes)),
            ])
        print_table(
            ["tool", "calls", "dur p50", "dur p90", "dur max", "out p50", "out max"], rows,
        )

        print("\nTool calls per assistant turn (parallelism, telemetry):")
        per_turn = Counter()
        for inv in data.invocations:
            turns = cakelib.group_by(inv.tool_calls, lambda t: t["turn_index"])
            for calls in turns.values():
                per_turn[len(calls)] += 1
        print_table(
            ["calls in turn", "turns", "share"],
            [[k, n, fmt_pct(n, sum(per_turn.values()))] for k, n in sorted(per_turn.items())],
        )


def main() -> None:
    ns = cakelib.build_arg_parser(__doc__).parse_args()
    run(cakelib.load(ns))


if __name__ == "__main__":
    main()
