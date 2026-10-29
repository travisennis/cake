#!/usr/bin/env python3
"""External policy-gate health from generic gate telemetry.

Current sidecars record one `gate_startup` per configured gate and one
`gate_decision` for every gate that participated in a check, including a
bypass, a disabled gate, and a failed gate. This report reads those records
directly; the historical `judge_attempt` and `type_safe_shadow` records are
reported separately by `judge.py` for older sidecars.

Metadata only: gate names, versions, modes, decisions, failure classes, and
latency. Command text, reasons, messages, and credentials never enter a row.
"""

from collections import Counter

import cakelib
from cakelib import fmt_int, fmt_ms, percentile, print_header, print_table

GATE_OUTCOMES = ("allow", "warn", "block", "abstain",
                 "failure", "disabled", "bypass")


def _entry(stats: dict, gate: str) -> dict:
    return stats.setdefault(gate, {
        "startups": 0,
        "startup_failures": 0,
        "decisions": 0,
        "outcomes": Counter(),
        "evidence": 0,
        "latency": [],
        "failure_classes": Counter(),
    })


def aggregate(data: cakelib.Dataset) -> dict[str, dict]:
    """Collect per-gate startup, decision, and evidence statistics."""
    stats: dict[str, dict] = {}
    for inv in data.invocations:
        for startup in inv.gate_startups:
            gate = startup.get("gate") or "(unknown)"
            entry = _entry(stats, gate)
            entry["startups"] += 1
            if startup.get("outcome") != "ready":
                entry["startup_failures"] += 1
                if failure := startup.get("failure_class"):
                    entry["failure_classes"][failure] += 1
        for decision in inv.gate_decisions:
            gate = decision.get("gate") or "(bypass)"
            entry = _entry(stats, gate)
            entry["decisions"] += 1
            entry["outcomes"][cakelib.gate_decision_outcome(decision)] += 1
            latency = decision.get("latency_ms")
            if latency is not None:
                entry["latency"].append(latency)
            if failure := decision.get("failure_class"):
                entry["failure_classes"][failure] += 1
        for evidence in inv.gate_evidence:
            gate = evidence.get("gate") or "(unknown)"
            _entry(stats, gate)["evidence"] += 1
    return stats


def _row(gate: str, entry: dict) -> list[str]:
    outcomes = entry["outcomes"]
    latency = entry["latency"]
    return [
        gate,
        fmt_int(entry["startups"]),
        fmt_int(entry["startup_failures"]),
        fmt_int(entry["decisions"]),
        fmt_int(outcomes.get("allow", 0)),
        fmt_int(outcomes.get("warn", 0)),
        fmt_int(outcomes.get("block", 0)),
        fmt_int(outcomes.get("abstain", 0)),
        fmt_int(outcomes.get("failure", 0) + outcomes.get("disabled", 0)),
        fmt_int(outcomes.get("bypass", 0)),
        fmt_int(entry["evidence"]),
        fmt_ms(percentile(latency, 50)),
        fmt_ms(percentile(latency, 90)),
    ]


def run(data: cakelib.Dataset) -> None:
    print_header("POLICY GATES")
    print(cakelib.describe_window(data))

    stats = aggregate(data)
    if not stats:
        print("\nNo generic gate_startup or gate_decision records in window.")
        return

    rows = []
    totals = {
        "startups": 0,
        "startup_failures": 0,
        "decisions": 0,
        "outcomes": Counter(),
        "evidence": 0,
        "latency": [],
        "failure_classes": Counter(),
    }
    for gate in sorted(stats):
        entry = stats[gate]
        rows.append(_row(gate, entry))
        for key in ("startups", "startup_failures", "decisions", "evidence"):
            totals[key] += entry[key]
        totals["outcomes"].update(entry["outcomes"])
        totals["latency"].extend(entry["latency"])
        totals["failure_classes"].update(entry["failure_classes"])
    rows.append(_row("TOTAL", totals))

    print("\nPer gate (startup, decisions, latency):")
    print_table(
        ["gate", "startups", "startup failures", "decisions", "allow", "warn",
         "block", "abstain", "failures/disabled", "bypass", "evidence",
         "p50", "p90"],
        rows,
    )

    if totals["failure_classes"]:
        print("\nFailures by class:")
        print_table(
            ["class", "events"],
            [[name, fmt_int(count)]
             for name, count in totals["failure_classes"].most_common()],
        )


def main() -> None:
    ns = cakelib.build_arg_parser(__doc__).parse_args()
    run(cakelib.load(ns))


if __name__ == "__main__":
    main()
