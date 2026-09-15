#!/usr/bin/env python3
"""Tool call metrics: volume, success/failure, failure taxonomy, retry recovery,
durations and output sizes, per-turn parallelism, and Bash `reason` coverage
with the judge outcomes it correlates with."""

from __future__ import annotations

from collections import Counter, defaultdict

import cakelib
from cakelib import (
    classify_tool_error, fmt_bytes, fmt_int, fmt_ms, fmt_pct, percentile,
    print_header, print_table,
)

FILE_TOOLS = ("Edit", "Write")

# Every judge outcome event kind shares this prefix. The current vocabulary is
# `judge_verdict`, `judge_fail_closed`, and `judge_bypass`; a judge kind added
# later is reported as `other` instead of being dropped from the join.
JUDGE_EVENT_KIND_PREFIX = "judge_"

# The bounded verdict vocabulary, and the judged outcomes the rates divide by:
# only a `judge_verdict` carries one.
JUDGE_VERDICTS = ("allow", "warn", "block")

# The full outcome column order, as (stats key, column label): the verdicts, the
# judge outcomes without a verdict, and the join's own bookkeeping (calls with
# no judge outcome split by cause, pairs that are not determinate, and values
# outside the vocabulary). The label lives next to the key it names, so an
# outcome cannot ship a column in a different position from the value the row
# puts under it.
JUDGE_OUTCOMES = (
    ("allow", "allow"),
    ("warn", "warn"),
    ("block", "block"),
    ("fail_closed", "fail-closed"),
    ("bypass", "bypass"),
    ("no_call_id", "no call id"),
    ("no_sidecar", "no sidecar"),
    ("no_event", "no event"),
    ("ambiguous", "ambiguous"),
    ("other", "other"),
)
JUDGE_OUTCOME_LABELS = dict(JUDGE_OUTCOMES)
JUDGE_OUTCOME_HEADERS = tuple(label for _key, label in JUDGE_OUTCOMES)


def bash_reason_coverage(data: cakelib.Dataset) -> list[list]:
    """Share of Bash calls that carry a parsed, non-empty `reason`, per model.

    Returns rows of [model, calls, with_reason, coverage] ordered by call
    volume, ending with a TOTAL row. Only the presence/absence of `reason` is
    counted (an empty string is absent); the reason text and the command never
    enter the report.
    """
    per_model: dict[str, list] = {}
    for s in data.sessions:
        for c in s.tool_calls_in_window(data.cutoff):
            if c.name != "Bash":
                continue
            stats = per_model.setdefault(s.model, [0, 0])
            stats[0] += 1  # calls
            if c.reason:
                stats[1] += 1  # with_reason
    rows = []
    for model, (calls, with_reason) in sorted(
        per_model.items(), key=lambda kv: (-kv[1][0], kv[0])
    ):
        rows.append([model, fmt_int(calls), fmt_int(with_reason),
                     fmt_pct(with_reason, calls)])
    total_calls = sum(stats[0] for stats in per_model.values())
    total_with = sum(stats[1] for stats in per_model.values())
    rows.append(["TOTAL", fmt_int(total_calls), fmt_int(total_with),
                 fmt_pct(total_with, total_calls)])
    return rows


def judge_event_outcome(event: dict) -> str:
    """Bucket one judge compensation event into the outcome vocabulary.

    Only a `judge_verdict` carries a bounded verdict (`allow`, `warn:<code>`, or
    `block:<code>`). `fail_closed` and `bypass` are judge outcomes with no
    verdict, and an unrecognized kind or verdict value is `other`, so a
    vocabulary change stays visible instead of riding an existing column.
    """
    kind = str(event.get("kind") or "")
    if kind == "judge_fail_closed":
        return "fail_closed"
    if kind == "judge_bypass":
        return "bypass"
    if kind == "judge_verdict":
        decision = (event.get("detail") or "").partition(":")[0]
        if decision in JUDGE_VERDICTS:
            return decision
    return "other"


def bash_judge_outcomes(data: cakelib.Dataset) -> tuple[list[list], dict]:
    """Judge outcomes for Bash calls with and without a `reason`, per model.

    Pairing is deterministic and order-independent: the raw `call_id` on the
    transcript's `function_call`/`function_call_output` Bash pair is hashed to
    the SHA-256 hex digest a judge compensation event carries in its `call_id`
    field (`cakelib.call_id_digest`), and the join is restricted to one session.
    A transcript records no invocation id, so scoping the join to
    (session, invocation) would mean pairing by timestamp or record position,
    which this join refuses to do. Timestamps and record order never
    participate, so concurrent and out-of-order records pair the same way.

    Per (model, reason presence) group the columns are the paired calls, the
    bounded verdicts (`allow`, `warn`, `block`), the judge outcomes without a
    verdict (`fail-closed`, `bypass`), the calls with no judge outcome split by
    cause --- `no call id` (the call has no provider-assigned identifier, so
    there is nothing to join on), `no sidecar` (the session has no telemetry
    records in the window), `no event` (the session has telemetry, but no judge
    event carries this call's digest: not judged, interrupted, or recorded
    before judge telemetry existed) --- `ambiguous` calls whose digest matches
    more than one call or more than one judge event in the session (excluded
    rather than guessed at), and `other` outcomes outside the vocabulary. Rates
    are the share of judged calls (`allow` + `warn` + `block`); the outcome
    columns sum to `calls`.

    Metadata only: the raw command, the reason text, and the raw call id never
    enter a row, and a judge retry contributes one outcome, because a judge
    event is recorded once per judged call however many attempts it took.

    Returns (rows, linkage): rows are one row per model and reason group plus a
    TOTAL row per group; `linkage` accounts for the join as a whole, including
    judge events with no transcript call and judge attempts per call.
    """
    # Transcript side: count Bash calls per (session, call-id digest) and keep
    # each call's group, so classification happens after the telemetry index is
    # complete. A call with no provider-assigned id cannot be linked.
    call_counts: Counter = Counter()
    groups: dict[tuple[str, str], Counter] = {}
    facts: list[tuple[tuple[str, str], tuple[str, str] | None]] = []
    for s in data.sessions:
        for c in s.tool_calls_in_window(data.cutoff):
            if c.name != "Bash":
                continue
            group = (s.model, "with reason" if c.reason else "without reason")
            groups.setdefault(group, Counter())["calls"] += 1
            key = (s.id, cakelib.call_id_digest(c.call_id)) if c.call_id else None
            if key is not None:
                call_counts[key] += 1
            facts.append((group, key))

    # Telemetry side: index judge events and judge attempts by the same key, and
    # record which sessions have telemetry at all, so a call with no judge event
    # can say whether its session has a sidecar.
    events: dict[tuple[str, str], list[str]] = defaultdict(list)
    attempts: Counter = Counter()
    unlinkable_events: Counter = Counter()
    sidecar_sessions: set[str] = set()
    for inv in data.invocations:
        sidecar_sessions.add(inv.session_id)
        for e in inv.compensations:
            if not str(e.get("kind") or "").startswith(JUDGE_EVENT_KIND_PREFIX):
                continue
            outcome = judge_event_outcome(e)
            digest = e.get("call_id")
            if digest:
                events[(inv.session_id, digest)].append(outcome)
            else:
                # A record written before the linkage field existed, or one
                # whose call had no provider-assigned identifier.
                unlinkable_events[outcome] += 1
        for a in inv.judge_attempts:
            digest = a.get("call_id")
            if digest:
                attempts[(inv.session_id, digest)] += 1

    # One call and one judge event per digest is the only determinate pair; more
    # of either is ambiguous and is counted, never guessed at. A call with no
    # judge event is attributed to the reason its session shows.
    paired = 0
    for group, key in facts:
        stats = groups[group]
        if key is None:
            stats["no_call_id"] += 1
            continue
        matched = events.get(key, [])
        if call_counts[key] > 1 or len(matched) > 1:
            stats["ambiguous"] += 1
        elif matched:
            stats[matched[0]] += 1
            paired += 1
        elif key[0] in sidecar_sessions:
            stats["no_event"] += 1
        else:
            stats["no_sidecar"] += 1

    # Event-side accounting: every judge event in the window is either paired to
    # exactly one transcript call (the same count as the paired calls), excluded
    # as an ambiguous pair, or unmatched --- for want of a linkage key, or
    # because its transcript call is not in the window.
    keyless_events = Counter(unlinkable_events)
    windowless_events = Counter()
    ambiguous_events = 0
    for key, outcomes in events.items():
        if key not in call_counts:
            windowless_events.update(outcomes)
        elif call_counts[key] > 1 or len(outcomes) > 1:
            ambiguous_events += len(outcomes)

    rows = []
    model_calls: Counter = Counter()
    for (model, _label), stats in groups.items():
        model_calls[model] += stats["calls"]
    ordered = sorted(
        groups.items(),
        key=lambda kv: (-model_calls[kv[0][0]], kv[0][0], kv[0][1] != "with reason"),
    )
    for (model, label), stats in ordered:
        rows.append(_judge_outcome_row(model, label, stats))

    totals: dict[str, Counter] = {}
    for (_model, label), stats in groups.items():
        totals.setdefault(label, Counter()).update(stats)
    for label in ("with reason", "without reason"):
        if label in totals:
            rows.append(_judge_outcome_row("TOTAL", label, totals[label]))

    linkage = {
        "calls": sum(stats["calls"] for stats in groups.values()),
        "paired": paired,
        "no_call_id": sum(stats["no_call_id"] for stats in groups.values()),
        "no_sidecar": sum(stats["no_sidecar"] for stats in groups.values()),
        "no_event": sum(stats["no_event"] for stats in groups.values()),
        "ambiguous": sum(stats["ambiguous"] for stats in groups.values()),
        "events": sum(len(v) for v in events.values()) + sum(keyless_events.values()),
        "events_paired": paired,
        "events_ambiguous": ambiguous_events,
        "events_keyless": keyless_events,
        "events_windowless": windowless_events,
        "attempts": sum(attempts.values()),
        "attempted_calls": len(attempts),
        "retried_calls": sum(1 for n in attempts.values() if n > 1),
    }
    return rows, linkage


def _judge_outcome_row(model: str, reason: str, stats: Counter) -> list[str]:
    judged = sum(stats[verdict] for verdict in JUDGE_VERDICTS)
    return [
        model,
        reason,
        fmt_int(stats["calls"]),
        *[fmt_int(stats[key]) for key, _label in JUDGE_OUTCOMES],
        fmt_pct(stats["block"], judged),
        fmt_pct(stats["warn"], judged),
    ]


def judge_linkage_notes(linkage: dict) -> list[str]:
    """Lines that keep the join's accounting and its rate denominator explicit."""

    def tally(counts: Counter) -> str:
        """An outcome breakdown, or `none` when there is nothing to break down."""
        return ", ".join(
            f"{JUDGE_OUTCOME_LABELS.get(outcome, outcome)} {fmt_int(n)}"
            for outcome, n in sorted(counts.items())
        ) or "none"

    unjudged = linkage["no_call_id"] + linkage["no_sidecar"] + linkage["no_event"]
    keyless = linkage["events_keyless"]
    windowless = linkage["events_windowless"]
    return [
        "Judge linkage (SHA-256 digest of the transcript call id, same session):",
        f"  Bash calls: {fmt_int(linkage['calls'])} (the coverage total above)"
        f" | paired: {fmt_int(linkage['paired'])}"
        f" | no judge outcome: {fmt_int(unjudged)} (no call id: {fmt_int(linkage['no_call_id'])}"
        f", no sidecar: {fmt_int(linkage['no_sidecar'])}"
        f", no event: {fmt_int(linkage['no_event'])})"
        f" | ambiguous: {fmt_int(linkage['ambiguous'])}",
        f"  Judge events: {fmt_int(linkage['events'])}"
        f" | paired: {fmt_int(linkage['events_paired'])}"
        f" | ambiguous: {fmt_int(linkage['events_ambiguous'])}"
        f" | no linkage key: {fmt_int(sum(keyless.values()))}"
        f" | call outside the window: {fmt_int(sum(windowless.values()))}",
        f"  Outcomes of the unpaired judge events: {tally(Counter(keyless) + Counter(windowless))}",
        f"  Judge attempts: {fmt_int(linkage['attempts'])} across"
        f" {fmt_int(linkage['attempted_calls'])} calls,"
        f" {fmt_int(linkage['retried_calls'])} retried",
        "  A retried call contributes one outcome; judge_attempt records carry their own digest, so they"
        " are counted even where a compensation event predates the linkage.",
        "  Rates are the share of judged calls (allow + warn + block), and the outcome columns sum to `calls`.",
        "  A call with no judge outcome was not judged, or was judged without leaving a linked event.",
    ]


def run(data: cakelib.Dataset) -> None:
    print_header("TOOL CALLS")
    print(cakelib.describe_window(data))

    all_calls = [(s, c) for s in data.sessions for c in s.tool_calls_in_window(data.cutoff)]
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

    print("\nBash `reason` coverage (calls with a parsed, non-empty `reason`, per model):")
    print_table(["model", "calls", "with reason", "coverage"], bash_reason_coverage(data))

    print("\nBash judge outcomes by `reason` presence:")
    outcome_rows, linkage = bash_judge_outcomes(data)
    print_table(
        ["model", "reason", "calls"] + list(JUDGE_OUTCOME_HEADERS) + ["block %", "warn %"],
        outcome_rows,
    )
    for line in judge_linkage_notes(linkage):
        print(line)

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
            calls = s.tool_calls_in_window(data.cutoff)
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
