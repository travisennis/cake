#!/usr/bin/env python3
"""Session inventory: activity over time, models, versions, projects, run modes."""

from collections import Counter
from statistics import median

import cakelib
from cakelib import fmt_bytes, fmt_int, fmt_pct, print_header, print_table


def run(data: cakelib.Dataset) -> None:
    print_header("OVERVIEW")
    print(cakelib.describe_window(data))
    sessions = data.sessions
    if not sessions:
        return

    with_sidecar = {inv.session_id for inv in data.invocations}
    covered = sum(1 for s in sessions if s.id in with_sidecar)
    print(f"Sessions with telemetry sidecar: {covered}/{len(sessions)} ({fmt_pct(covered, len(sessions))})")
    if data.session_parse_errors or data.telemetry_parse_errors:
        print(f"Unparseable lines skipped: sessions={data.session_parse_errors} telemetry={data.telemetry_parse_errors}")

    print("\nActivity by day:")
    by_day = cakelib.group_by(sessions, lambda s: s.mtime.date().isoformat())
    rows = []
    for day in sorted(by_day):
        day_sessions = by_day[day]
        tool_calls = sum(len(s.tool_calls) for s in day_sessions)
        tasks = sum(len(s.by_type("task_start", "result")) for s in day_sessions)
        rows.append([day, len(day_sessions), fmt_int(tasks), fmt_int(tool_calls)])
    print_table(["date", "sessions", "tasks", "tool calls"], rows)

    print("\nModels:")
    models = Counter(s.model for s in sessions)
    print_table(
        ["model", "sessions", "share"],
        [[m, n, fmt_pct(n, len(sessions))] for m, n in models.most_common()],
    )

    print("\ncake versions / transcript format versions:")
    versions = Counter((s.cake_version, s.format_version) for s in sessions)
    print_table(
        ["cake version", "format", "sessions"],
        [[v, fv if fv is not None else "-", n] for (v, fv), n in versions.most_common()],
    )

    if data.invocations:
        print("\nRun modes (telemetry):")
        modes = Counter((inv.init or {}).get("mode", "unknown") for inv in data.invocations)
        print_table(["mode", "invocations"], [[m, n] for m, n in modes.most_common()])

        apis = Counter(
            ((inv.init or {}).get("api_type", "?"), (inv.init or {}).get("output_format", "?"))
            for inv in data.invocations
        )
        print("\nAPI type / output format (telemetry):")
        print_table(["api type", "output format", "invocations"], [[a, o, n] for (a, o), n in apis.most_common()])

    print("\nTop projects (working directory):")
    projects = Counter(s.working_directory for s in sessions)
    print_table(["working directory", "sessions"], [[wd, n] for wd, n in projects.most_common(10)])

    print("\nPer-session shape (median):")
    rows = [
        ["file size", fmt_bytes(median(s.size for s in sessions))],
        ["records", fmt_int(median(len(s.records) for s in sessions))],
        ["user messages", fmt_int(median(len([m for m in s.by_type('message') if m.get('role') == 'user']) for s in sessions))],
        ["assistant messages", fmt_int(median(len([m for m in s.by_type('message') if m.get('role') == 'assistant']) for s in sessions))],
        ["tool calls", fmt_int(median(len(s.tool_calls) for s in sessions))],
        ["tasks", fmt_int(median(len(s.by_type('task_start', 'result')) for s in sessions))],
    ]
    print_table(["metric", "median"], rows)


def main() -> None:
    ns = cakelib.build_arg_parser(__doc__).parse_args()
    run(cakelib.load(ns))


if __name__ == "__main__":
    main()
