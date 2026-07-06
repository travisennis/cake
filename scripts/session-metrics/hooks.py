#!/usr/bin/env python3
"""Hook and skill metrics: hook events, decisions, denials, failures, durations,
and skill activations.

Source: transcript hook_event and skill_activated records.
"""

from collections import Counter

import cakelib
from cakelib import fmt_int, fmt_ms, fmt_pct, percentile, print_header, print_table


def run(data: cakelib.Dataset) -> None:
    print_header("HOOKS AND SKILLS")
    print(cakelib.describe_window(data))

    events = [rec for s in data.sessions for rec in s.by_type("hook_event")]
    if events:
        print(f"\nHook events: {fmt_int(len(events))}")
        by_event = Counter(e.get("event", "?") for e in events)
        print_table(["event", "count"], [[e, n] for e, n in by_event.most_common()])

        print("\nDecisions (decision -> resolved):")
        decisions = Counter(
            (e.get("decision") or "-", e.get("resolved_decision") or "-") for e in events
        )
        print_table(
            ["decision", "resolved", "count", "share"],
            [[d, r, n, fmt_pct(n, len(events))] for (d, r), n in decisions.most_common()],
        )

        denies = [e for e in events if e.get("decision") == "deny" or e.get("resolved_decision") == "deny"]
        if denies:
            print(f"\nDenied tool calls: {fmt_int(len(denies))} - top denied tools/commands:")
            top = Counter(
                (e.get("tool_name") or "?", (e.get("tool_input_summary") or "")[:60]) for e in denies
            )
            print_table(["tool", "input", "count"], [[t, i, n] for (t, i), n in top.most_common(8)])

        hook_errors = [e for e in events if e.get("decision") == "error"]
        fail_closed = [e for e in hook_errors if e.get("fail_closed")]
        nonzero_exits = Counter(
            e.get("exit_code") for e in events if e.get("exit_code") not in (0, None)
        )
        print(f"\nHook infrastructure errors: {fmt_int(len(hook_errors))} "
              f"(fail-closed: {fmt_int(len(fail_closed))})")
        if nonzero_exits:
            print_table(["exit code", "count"], [[c, n] for c, n in nonzero_exits.most_common()])

        durations = [e["duration_ms"] for e in events if e.get("duration_ms") is not None]
        if durations:
            print(f"\nHook duration: p50 {fmt_ms(percentile(durations, 50))} | "
                  f"p90 {fmt_ms(percentile(durations, 90))} | max {fmt_ms(max(durations))}")

        print("\nTop hook commands:")
        commands = Counter((e.get("command") or "?")[:70] for e in events)
        print_table(["command", "count"], [[c, n] for c, n in commands.most_common(8)])
    else:
        print("\nNo hook events in window.")

    skills = [rec for s in data.sessions for rec in s.by_type("skill_activated")]
    if skills:
        print(f"\nSkill activations: {fmt_int(len(skills))}")
        by_name = Counter(sk.get("name", "?") for sk in skills)
        sessions_using = sum(1 for s in data.sessions if s.by_type("skill_activated"))
        print_table(["skill", "activations"], [[n, c] for n, c in by_name.most_common()])
        print(f"Sessions using skills: {fmt_int(sessions_using)} "
              f"({fmt_pct(sessions_using, len(data.sessions))})")
    else:
        print("\nNo skill activations in window.")


def main() -> None:
    ns = cakelib.build_arg_parser(__doc__).parse_args()
    run(cakelib.load(ns))


if __name__ == "__main__":
    main()
