#!/usr/bin/env python3
"""Task and invocation outcomes: success rates, durations, turns, permission
denials by source, and abnormal terminations.

Sources: transcript task_complete/result records and telemetry session_summary.
"""

import re
from collections import Counter, defaultdict

import cakelib
from cakelib import fmt_int, fmt_ms, fmt_pct, percentile, print_header, print_table

# ---------------------------------------------------------------------------
# Permission-denial attribution
# ---------------------------------------------------------------------------
# Every `permission_denials` label is `{tool}({call_id}): {text}`, and the text
# after that separator identifies the gate that denied the call, by this
# precedence:
#
#   `judge block: `       -> judge-block        (judge_denial_label,
#                                                src/clients/agent/agent_loop.rs)
#   `judge fail-closed: ` -> judge-fail-closed  (same)
#   `sandbox: `           -> sandbox            (SandboxPathRef::permission_label,
#                                                src/clients/tools/bash.rs)
#   `path: `              -> path               (the in-process path gate)
#   `{hook_path}: {rest}` -> hook, unless `rest` itself begins with one of the
#                            tokens above, which then wins
#   anything else         -> other
#
# A hook denial is `{tool}({call_id}): {hook_path}: {reason}`, and `{reason}` is
# hook-supplied text (src/hooks.rs), so a reason that itself begins with a cake
# token is attributed to that token's source rather than to `hook`. That is
# recorded behavior, not an accident to correct here: the label content is a
# compatibility surface, so this report pins the precedence with tests instead
# of changing what cake writes. A label without the separator, or with text in
# none of these shapes, is counted as `other` rather than dropped --- the same
# accounting discipline the judge linkage table in `tools.py` uses.
#
# Metadata only: the source name is the only label-derived value that enters the
# report. The reason text, the denied path, and the command never do.
DENIAL_SOURCES = (
    "judge-block", "judge-fail-closed", "sandbox", "path", "hook", "other",
)
_DENIAL_TOKENS = (
    ("judge block: ", "judge-block"),
    ("judge fail-closed: ", "judge-fail-closed"),
    ("sandbox: ", "sandbox"),
    ("path: ", "path"),
)
_DENIAL_SEPARATOR = re.compile(r"^[^()]+\([^()]*\): ")


def _token_source(text: str) -> str | None:
    for prefix, source in _DENIAL_TOKENS:
        if text.startswith(prefix):
            return source
    return None


def denial_source(label: object) -> str:
    """The gate a `permission_denials` label is attributed to."""
    if not isinstance(label, str):
        return "other"
    separator = _DENIAL_SEPARATOR.match(label)
    if separator is None:
        return "other"
    text = label[separator.end():]
    source = _token_source(text)
    if source is not None:
        return source
    head, sep, rest = text.partition(": ")
    if not sep or not head:
        return "other"
    # The hook shape is `{hook_path}: {reason}`. The reason's own leading token
    # wins, which is the ambiguity documented above.
    return _token_source(rest) or "hook"


def denial_counts(tasks: list[dict]) -> tuple[int, int, Counter, Counter]:
    """Event and task counts for the in-window completed tasks.

    Returns (events, denied_tasks, events_by_source, tasks_by_source).
    `events_by_source` sums to `events`, so the breakdown reconciles with the
    flat total; `tasks_by_source` counts a task once per source it carries, so
    a task denied by two gates appears under both.
    """
    events = 0
    denied_tasks = 0
    by_source: Counter = Counter()
    tasks_by_source: Counter = Counter()
    for task in tasks:
        labels = task.get("permission_denials")
        if not labels:
            continue
        if not isinstance(labels, list):
            labels = [labels]
        denied_tasks += 1
        events += len(labels)
        sources = Counter(denial_source(label) for label in labels)
        by_source.update(sources)
        tasks_by_source.update(sources.keys())
    return events, denied_tasks, by_source, tasks_by_source


def denial_rows(tasks: list[dict]) -> tuple[list[list[str]], int, int]:
    """The per-source table for the in-window tasks, plus its flat totals.

    Returns (rows, events, denied_tasks). Rows are ordered by event count
    (ties in vocabulary order) and end with a TOTAL row, so the parts visibly
    sum to the flat total. Sources with no events get no row; `other` appears
    whenever a label shape was outside the vocabulary, so nothing is dropped
    silently.
    """
    total, denied_tasks, by_source, tasks_by_source = denial_counts(tasks)
    rows = [
        [source, fmt_int(by_source[source]), fmt_int(tasks_by_source[source])]
        for source in sorted(
            by_source, key=lambda s: (-by_source[s], DENIAL_SOURCES.index(s))
        )
    ]
    if rows:
        rows.append(["TOTAL", fmt_int(total), fmt_int(denied_tasks)])
    return rows, total, denied_tasks


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

        denial_table, total, denied_tasks = denial_rows(tasks)
        print(f"\nPermission denials recorded: {fmt_int(total)} across "
              f"{fmt_int(denied_tasks)} tasks")
        if denial_table:
            print_table(["source", "events", "tasks"], denial_table)

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

        # Turn totals per session: continue/resume/fork invocations each write
        # their own session_summary to the same sidecar, and turn_count is
        # cumulative within one invocation, so the session total is the sum
        # across its invocations. Transcripts cannot be split that way, so
        # this is telemetry-only, like the other per-invocation reports.
        per_session: dict[str, list[int]] = defaultdict(list)
        for inv in summaries:
            per_session[inv.session_id].append(inv.summary["turn_count"])
        per_session_turns = sorted(
            ((sid, len(invs), sum(invs)) for sid, invs in per_session.items()),
            key=lambda row: (-row[2], row[0]),
        )
        turn_totals = [row[2] for row in per_session_turns]
        print("\nTurns per session (telemetry, summed across invocations):")
        print_table(
            ["metric", "p50", "p90", "max"],
            [["turns/session", fmt_int(percentile(turn_totals, 50)),
              fmt_int(percentile(turn_totals, 90)), fmt_int(max(turn_totals))]],
        )
        print_table(
            ["session", "invocations", "turns"],
            [[row[0], fmt_int(row[1]), fmt_int(row[2])] for row in per_session_turns],
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
