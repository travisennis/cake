#!/usr/bin/env python3
"""Where invocation wall-clock time goes: model API vs tool execution vs retry
waits vs unaccounted overhead, plus turn pacing, think time between tasks, and
the slowest individual operations.

Wall time comes from telemetry session_summary; API time from api_attempt
total_ms (request + response parsing); tool time from tool_call duration_ms;
retry waits from retry_scheduled delay_ms. Hook time is reported from
transcript hook_event records (it overlaps the tool path, so it is shown for
scale, not added to the breakdown). Think time is the transcript gap between a
task_complete and the next task_start in the same session.
"""

import cakelib
from cakelib import fmt_bytes, fmt_int, fmt_ms, fmt_pct, percentile, print_header, print_table


def run(data: cakelib.Dataset) -> None:
    print_header("TIME BREAKDOWN")
    print(cakelib.describe_window(data))

    complete = [inv for inv in data.invocations if inv.summary]
    if not complete:
        print("\nNo telemetry session_summary records in window.")
        return

    wall = api = tools = retry_wait = parse = 0
    for inv in complete:
        wall += inv.summary["duration_ms"]
        api += sum(a.get("total_ms", 0) for a in inv.attempts)
        parse += sum(a.get("parse_ms", 0) for a in inv.attempts)
        tools += sum(t.get("duration_ms", 0) for t in inv.tool_calls)
        retry_wait += sum(r.get("delay_ms", 0) for r in inv.retries)
    other = max(0, wall - api - tools - retry_wait)

    print(f"\nAcross {fmt_int(len(complete))} invocations "
          f"({fmt_ms(wall)} total wall time):")
    print_table(
        ["where", "time", "share of wall"],
        [
            ["model API (request+parse)", fmt_ms(api), fmt_pct(api, wall)],
            ["  of which response parsing", fmt_ms(parse), fmt_pct(parse, wall)],
            ["tool execution", fmt_ms(tools), fmt_pct(tools, wall)],
            ["retry backoff waits", fmt_ms(retry_wait), fmt_pct(retry_wait, wall)],
            ["other (streaming, transcript writes, ...)", fmt_ms(other), fmt_pct(other, wall)],
        ],
    )

    print("\nTool time by tool:")
    tel_calls = [tc for inv in complete for tc in inv.tool_calls]
    rows = []
    for tool, calls in sorted(cakelib.group_by(tel_calls, lambda t: t["name"]).items(),
                              key=lambda kv: -sum(c.get("duration_ms", 0) for c in kv[1])):
        tool_time = sum(c.get("duration_ms", 0) for c in calls)
        rows.append([tool, fmt_int(len(calls)), fmt_ms(tool_time),
                     fmt_pct(tool_time, tools), fmt_pct(tool_time, wall)])
    print_table(["tool", "calls", "time", "share of tool time", "share of wall"], rows)

    print("\nAPI time by model:")
    rows = []
    by_model = cakelib.group_by(complete, lambda inv: inv.model)
    for model, invs in sorted(by_model.items(), key=lambda kv: -len(kv[1])):
        model_api = sum(a.get("total_ms", 0) for inv in invs for a in inv.attempts)
        attempts = sum(len(inv.attempts) for inv in invs)
        rows.append([model, fmt_int(attempts), fmt_ms(model_api), fmt_pct(model_api, api)])
    print_table(["model", "attempts", "time", "share of API time"], rows)

    hook_time = sum(
        e.get("duration_ms") or 0 for s in data.sessions for e in s.by_type("hook_event")
    )
    if hook_time:
        print(f"\nHook execution (transcripts, overlaps tool path): {fmt_ms(hook_time)}")

    print("\nTurn pacing (wall per turn, per invocation):")
    pace = [inv.summary["duration_ms"] / inv.summary["turn_count"]
            for inv in complete if inv.summary.get("turn_count")]
    print_table(["p50", "p90", "max"], [[
        fmt_ms(percentile(pace, 50)), fmt_ms(percentile(pace, 90)), fmt_ms(max(pace, default=0)),
    ]])

    # Think time: gap between one task ending and the next starting.
    # task_complete has no timestamp, so a task's end is its task_start
    # timestamp plus the task_complete duration_ms.
    gaps = []
    for s in data.sessions:
        durations = {r.get("task_id"): r.get("duration_ms")
                     for r in s.by_type("task_complete")}
        starts = [(cakelib.parse_ts(r.get("timestamp")), r.get("task_id"))
                  for r in s.by_type("task_start")]
        starts = [(ts, tid) for ts, tid in starts if ts is not None]
        for (start, task_id), (next_start, _) in zip(starts, starts[1:]):
            duration = durations.get(task_id)
            if duration is None:
                continue
            gap = (next_start - start).total_seconds() * 1000 - duration
            if gap >= 0:
                gaps.append(gap)
    if gaps:
        print(f"\nThink time between tasks (n={fmt_int(len(gaps))}, "
              f"total {fmt_ms(sum(gaps))}):")
        print_table(["p50", "p90", "max"], [[
            fmt_ms(percentile(gaps, 50)), fmt_ms(percentile(gaps, 90)), fmt_ms(max(gaps)),
        ]])

    print("\nSlowest tool calls:")
    slowest = sorted(tel_calls, key=lambda t: -t.get("duration_ms", 0))[:5]
    print_table(
        ["tool", "duration", "output", "session"],
        [[t["name"], fmt_ms(t["duration_ms"]), fmt_bytes(t.get("output_bytes", 0)),
          t.get("session_id", "?")[:8]] for t in slowest],
    )

    print("\nSlowest API attempts:")
    all_attempts = [(inv, a) for inv in complete for a in inv.attempts]
    slowest = sorted(all_attempts, key=lambda ia: -ia[1].get("total_ms", 0))[:5]
    print_table(
        ["model", "duration", "input tokens", "session"],
        [[inv.model, fmt_ms(a["total_ms"]),
          fmt_int((a.get("usage") or {}).get("input_tokens", 0)),
          inv.session_id[:8]] for inv, a in slowest],
    )


def main() -> None:
    ns = cakelib.build_arg_parser(__doc__).parse_args()
    run(cakelib.load(ns))


if __name__ == "__main__":
    main()
