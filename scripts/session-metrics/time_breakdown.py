#!/usr/bin/env python3
"""Invocation wall-clock timelines alongside aggregate API, tool, and retry
measurements, turn pacing, and the slowest individual operations.

Wall time comes from telemetry session_summary. New sidecars carry tool spans
and observed retry waits; API starts come from api_attempt_in_flight. These
permit an exclusive, interval-based timeline for each invocation while old
records remain unobserved. Aggregate tool work still overlaps and scheduled
retry delay is not observed wait. Hook time comes from transcript hook_event
records and is shown separately because it can overlap the tool path.
"""

from collections import defaultdict

import cakelib
from cakelib import fmt_bytes, fmt_int, fmt_ms, fmt_pct, percentile, print_header, print_table


TIMELINE_CATEGORIES = (
    "main provider", "retry wait", "delegated agent", "Bash safety models",
    "Bash remaining", "other tools", "parallel activity", "unobserved",
)


def _ms(value: str | None) -> float | None:
    parsed = cakelib.parse_ts(value)
    return parsed.timestamp() * 1000 if parsed is not None else None


def _span(record: dict) -> tuple[float, float] | None:
    start, end = _ms(record.get("started_at")), _ms(record.get("completed_at"))
    if start is None or end is None or end < start:
        return None
    return start, end


def invocation_timeline(inv: cakelib.Invocation) -> tuple[dict[str, float], dict[str, int]] | None:
    """Assign each observed instant once; keep missing historical spans visible."""
    if not inv.summary or (end := _ms(inv.summary.get("timestamp"))) is None:
        return None
    wall = inv.summary.get("duration_ms") or 0
    begin = end - wall
    spans: list[tuple[float, float, str]] = []
    missing = defaultdict(int)

    def add(start: float, stop: float, category: str) -> None:
        start, stop = max(start, begin), min(stop, end)
        if stop > start:
            spans.append((start, stop, category))

    # One in-flight attempt may have two phase records. Its earliest start is
    # the request start; the completed attempt's timestamp is its end.
    api_starts = {}
    for record in inv.other:
        if record.get("type") != "api_attempt_in_flight":
            continue
        key = (record.get("turn_index"), record.get("attempt"))
        start = _ms(record.get("started_at"))
        if start is not None:
            api_starts[key] = min(start, api_starts.get(key, start))
    for attempt in inv.attempts:
        key = (attempt.get("turn_index"), attempt.get("attempt"))
        start, stop = api_starts.get(key), _ms(attempt.get("timestamp"))
        if start is None or stop is None or stop < start:
            missing["provider attempts"] += 1
        else:
            add(start, stop, "main provider")

    for wait in inv.retry_waits:
        if (span := _span(wait)) is None:
            missing["retry waits"] += 1
        else:
            add(*span, "retry wait")
    if inv.retries and not inv.retry_waits:
        missing["retry waits"] += len(inv.retries)

    # Judge attempts and TypeSafe observations identify the model time inside
    # a Bash call. Shadow observations may overlap judging, so the reported
    # safety slice uses the larger of the two totals, a conservative estimate.
    judge_ms = defaultdict(int)
    shadow_ms = defaultdict(int)
    for attempt in inv.judge_attempts:
        if digest := attempt.get("call_id"):
            judge_ms[digest] += (attempt.get("total_ms") or 0) + (attempt.get("retry_delay_ms") or 0)
    for observation in inv.other:
        if observation.get("type") == "type_safe_shadow" and (digest := observation.get("call_id")):
            shadow_ms[digest] += observation.get("elapsed_ms") or 0

    for tool in inv.tool_calls:
        if (span := _span(tool)) is None:
            missing["tool calls"] += 1
            continue
        start, stop = span
        name = tool.get("name")
        if name == "tb__subagent":
            add(start, stop, "delegated agent")
        elif name == "Bash":
            digest = cakelib.call_id_digest(tool.get("call_id", ""))
            safety = min(stop - start, max(judge_ms[digest], shadow_ms[digest]))
            add(start, start + safety, "Bash safety models")
            add(start + safety, stop, "Bash remaining")
        else:
            add(start, stop, "other tools")

    totals = {category: 0.0 for category in TIMELINE_CATEGORIES}
    boundaries = {begin, end}
    for start, stop, _ in spans:
        boundaries.update((start, stop))
    boundaries = sorted(boundaries)
    for left, right in zip(boundaries, boundaries[1:]):
        active = [category for start, stop, category in spans if start < right and stop > left]
        category = "unobserved" if not active else active[0] if len(active) == 1 else "parallel activity"
        totals[category] += right - left
    return totals, dict(missing)


def print_invocation_timelines(data: cakelib.Dataset) -> None:
    complete = [inv for inv in data.invocations if inv.summary]
    selected = data.selected_invocation
    shown = complete if selected else sorted(
        complete, key=lambda inv: -(inv.summary.get("duration_ms") or 0)
    )[:5]
    print("\nPer-invocation wall time (selected invocations, or five slowest):")
    for inv in shown:
        result = invocation_timeline(inv)
        if result is None:
            print(f"  {inv.session_id} / {inv.invocation_id}: precise timeline unavailable")
            continue
        totals, missing = result
        print(f"\n  {inv.session_id} / {inv.invocation_id}  "
              f"wall {fmt_ms(inv.summary['duration_ms'])}")
        print_table(["activity", "exclusive wall time"], [
            [category, fmt_ms(totals[category])]
            for category in TIMELINE_CATEGORIES if totals[category] >= 1
        ])
        if missing:
            print("  Missing spans: " + ", ".join(
                f"{count} {name}" for name, count in sorted(missing.items())
            ))
        slow = sorted(
            [(t.get("duration_ms") or 0, t.get("name", "tool")) for t in inv.tool_calls]
            + [(a.get("total_ms") or 0, "main provider") for a in inv.attempts],
            reverse=True,
        )[:3]
        if slow:
            print("  Slowest operations: " + ", ".join(
                f"{name} {fmt_ms(duration)}" for duration, name in slow
            ))
    print("\nBash safety model time is estimated within each Bash span; Bash remaining "
          "also includes command setup, execution, output handling, and hooks. "
          "Parallel activity is counted once. Unobserved includes setup, "
          "unmeasured waits, and historical records without spans.")


def run(data: cakelib.Dataset) -> None:
    print_header("TIME BREAKDOWN")
    print(cakelib.describe_window(data))

    if not data.invocations:
        print("\nNo telemetry invocations in window.")
        return

    print_invocation_timelines(data)

    complete = [inv for inv in data.invocations if inv.summary]
    incomplete = len(data.invocations) - len(complete)
    wall = sum(inv.summary["duration_ms"] for inv in complete)
    api = tools = retry_wait = parse = 0
    for inv in data.invocations:
        api += sum(a.get("total_ms", 0) for a in inv.attempts)
        parse += sum(a.get("parse_ms", 0) for a in inv.attempts)
        tools += sum(t.get("duration_ms", 0) for t in inv.tool_calls)
        retry_wait += sum(r.get("delay_ms", 0) for r in inv.retries)

    if complete:
        print(f"\nAcross {fmt_int(len(complete))} complete invocations: "
              f"{fmt_ms(wall)} total wall time.")
    else:
        print("\nNo complete invocation wall time is available in this window.")
    print(f"\nRecorded activity across {fmt_int(len(data.invocations))} telemetry "
          "invocations (includes incomplete invocations):")
    print_table(
        ["activity", "recorded duration", "interpretation"],
        [
            ["model API (request+parse)", fmt_ms(api), "elapsed per attempt"],
            ["  of which response parsing", fmt_ms(parse), "subset of API"],
            ["tool execution (cumulative work)", fmt_ms(tools), "may overlap"],
            ["scheduled retry delay", fmt_ms(retry_wait), "actual wait not measured"],
        ],
    )
    print("\nAggregate durations are cumulative work, so they cannot be subtracted "
          "from wall time. Use the per-invocation timeline for exclusive "
          "attribution when span records are available.")
    if incomplete:
        noun = "invocation" if incomplete == 1 else "invocations"
        verb = "has" if incomplete == 1 else "have"
        print(f"{fmt_int(incomplete)} incomplete {noun} {verb} no session_summary; "
              "only their wall time and turn pacing are unavailable.")

    print("\nCumulative tool work by tool:")
    tel_calls = [tc for inv in data.invocations for tc in inv.tool_calls]
    rows = []
    for tool, calls in sorted(cakelib.group_by(tel_calls, lambda t: t["name"]).items(),
                              key=lambda kv: -sum(c.get("duration_ms", 0) for c in kv[1])):
        tool_time = sum(c.get("duration_ms", 0) for c in calls)
        rows.append([tool, fmt_int(len(calls)), fmt_ms(tool_time),
                     fmt_pct(tool_time, tools)])
    print_table(
        ["tool", "calls", "cumulative time", "share of cumulative tool work"],
        rows,
    )

    print("\nAPI time by model:")
    rows = []
    by_model = cakelib.group_by(data.invocations, lambda inv: inv.model)
    for model, invs in sorted(by_model.items(), key=lambda kv: -len(kv[1])):
        model_api = sum(a.get("total_ms", 0) for inv in invs for a in inv.attempts)
        attempts = sum(len(inv.attempts) for inv in invs)
        rows.append([model, fmt_int(attempts), fmt_ms(model_api), fmt_pct(model_api, api)])
    print_table(["model", "attempts", "time", "share of API time"], rows)

    hook_time = sum(
        e.get("duration_ms") or 0
        for s in data.sessions for e in s.records_in_window(data.cutoff, "hook_event")
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
        starts = [(ts, tid) for ts, tid in starts
                  if ts is not None and (data.cutoff is None or ts >= data.cutoff)]
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
    all_attempts = [(inv, a) for inv in data.invocations for a in inv.attempts]
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
