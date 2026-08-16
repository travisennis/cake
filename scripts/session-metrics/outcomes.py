#!/usr/bin/env python3
"""Task and invocation outcomes: success rates, durations, turns, permission
denials, and abnormal terminations.

Sources: transcript task_complete/result records and telemetry session_summary.
"""

from collections import Counter

import cakelib
from cakelib import fmt_int, fmt_ms, fmt_pct, percentile, print_header, print_table


def run(data: cakelib.Dataset) -> None:
    print_header("OUTCOMES")
    print(cakelib.describe_window(data))

    tasks = [rec for s in data.sessions for rec in s.tasks_in_window(data.cutoff)]
    inflight = [s for s in data.sessions if s.inflight_in_window(data.cutoff)]
    if inflight:
        print(f"\nSessions with an incomplete final task (live/crashed/abandoned, "
              f"excluded from the completed-task counts below): {fmt_int(len(inflight))}")
    if tasks:
        ok = [t for t in tasks if t.get("subtype") == "success"]
        print(f"\nTasks completed (transcripts): {fmt_int(len(tasks))} | "
              f"success: {fmt_int(len(ok))} ({fmt_pct(len(ok), len(tasks))})")

        subtypes = Counter(t.get("subtype", "?") for t in tasks)
        print_table(["subtype", "count"], [[s, n] for s, n in subtypes.most_common()])

        durations = [t["duration_ms"] for t in tasks if t.get("duration_ms") is not None]
        turns = [t.get("num_turns") or t.get("turn_count") or 0 for t in tasks]
        tool_counts = [t["tool_call_count"] for t in tasks if t.get("tool_call_count") is not None]
        print("\nPer-task shape:")
        rows = [
            ["duration", fmt_ms(percentile(durations, 50)), fmt_ms(percentile(durations, 90)),
             fmt_ms(max(durations, default=0))],
            ["turns", fmt_int(percentile(turns, 50)), fmt_int(percentile(turns, 90)),
             fmt_int(max(turns, default=0))],
        ]
        if tool_counts:
            rows.append(["tool calls", fmt_int(percentile(tool_counts, 50)),
                         fmt_int(percentile(tool_counts, 90)), fmt_int(max(tool_counts))])
        print_table(["metric", "p50", "p90", "max"], rows)

        denials = [t.get("permission_denials") for t in tasks if t.get("permission_denials")]
        flat = [d for group in denials for d in (group if isinstance(group, list) else [group])]
        print(f"\nPermission denials recorded: {fmt_int(len(flat))} across "
              f"{fmt_int(len(denials))} tasks")

        print("\nTasks per session (completed, in window):")
        per_session = Counter(len(s.tasks_in_window(data.cutoff)) for s in data.sessions)
        print_table(
            ["tasks", "sessions"],
            [[k, n] for k, n in sorted(per_session.items())],
        )

    summaries = [inv for inv in data.invocations if inv.summary]
    if summaries:
        ok = [inv for inv in summaries if inv.summary.get("success")]
        print(f"\nInvocation summaries (telemetry): {fmt_int(len(summaries))} | "
              f"success: {fmt_int(len(ok))} ({fmt_pct(len(ok), len(summaries))})")

        errors = Counter(
            (inv.summary.get("error") or "").splitlines()[0][:70]
            for inv in summaries if not inv.summary.get("success")
        )
        if errors:
            print("\nFailure reasons:")
            print_table(["error", "count"], [[e or "(none)", n] for e, n in errors.most_common(8)])

        durations = [inv.summary["duration_ms"] for inv in summaries]
        turns = [inv.summary["turn_count"] for inv in summaries]
        print("\nPer-invocation shape:")
        print_table(
            ["metric", "p50", "p90", "max"],
            [
                ["duration", fmt_ms(percentile(durations, 50)), fmt_ms(percentile(durations, 90)),
                 fmt_ms(max(durations))],
                ["turns", fmt_int(percentile(turns, 50)), fmt_int(percentile(turns, 90)),
                 fmt_int(max(turns))],
            ],
        )

    # Invocations that started but never wrote a summary: crash/kill candidates.
    unterminated = [inv for inv in data.invocations if inv.init and not inv.summary]
    if data.invocations:
        print(f"\nInvocations without a session_summary (abnormal termination candidates): "
              f"{fmt_int(len(unterminated))} ({fmt_pct(len(unterminated), len(data.invocations))})")


def main() -> None:
    ns = cakelib.build_arg_parser(__doc__).parse_args()
    run(cakelib.load(ns))


if __name__ == "__main__":
    main()
