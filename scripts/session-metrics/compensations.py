#!/usr/bin/env python3
"""Model-compensation metrics: how often cake rescues each model weakness.

Every compensation counter maps to hand-coded knowledge that rescues a model
weakness. A counter that flatlines at zero for a given model is the signal
that the compensation is a deletion candidate: review the compensation and
delete it (or rework the prompt) before it becomes unmeasured cruft.

Source: telemetry `compensation` records, plus context-overflow retries
derived from `retry_scheduled` records (reason=context_overflow).
"""

from collections import Counter

import cakelib
from cakelib import fmt_int, fmt_ms, percentile, print_header, print_table

# The stable counter vocabulary, in the order shown in the per-model table.
KINDS = [
    "json_repair",
    "judge_verdict",
    "judge_fail_closed",
    "same_path_serialization",
    "output_truncation",
    "edit_invalid_arguments",
]


def count_events(data: cakelib.Dataset) -> tuple[Counter, Counter, list[int], Counter]:
    """Aggregate compensation events for testing.

    Returns (by_model, by_kind_detail, judge_latencies, overflow_by_model):
    - by_model maps model -> Counter(kind -> count)
    - by_kind_detail maps (kind, detail) -> count
    - judge_latencies lists judge_verdict latency_ms values
    - overflow_by_model maps model -> context-overflow retry count
    """
    by_model: dict[str, Counter] = {}
    by_kind_detail: Counter = Counter()
    judge_latencies: list[int] = []

    for inv in data.invocations:
        for c in inv.compensations:
            kind = c.get("kind", "?")
            detail = c.get("detail") or "-"
            by_model.setdefault(inv.model, Counter())[kind] += 1
            by_kind_detail[(kind, detail)] += 1
            if kind == "judge_verdict" and c.get("latency_ms") is not None:
                judge_latencies.append(c["latency_ms"])

    overflow_by_model: Counter = Counter()
    for inv in data.invocations:
        for r in inv.retries:
            if r.get("reason") == "context_overflow":
                overflow_by_model[inv.model] += 1

    return by_model, by_kind_detail, judge_latencies, overflow_by_model


def run(data: cakelib.Dataset) -> None:
    print_header("MODEL COMPENSATIONS")
    print(cakelib.describe_window(data))

    by_model, by_kind_detail, judge_latencies, overflow_by_model = count_events(data)
    all_models = sorted(set(by_model) | set(overflow_by_model))

    if not all_models:
        print("\nNo compensation events in window.")
        return

    print("\nPer model (compensation events):")
    rows = []
    for model in all_models:
        counts = by_model.get(model, Counter())
        overflow = overflow_by_model.get(model, 0)
        rows.append(
            [model]
            + [fmt_int(counts.get(kind, 0)) for kind in KINDS]
            + [fmt_int(overflow)]
            + [fmt_int(sum(counts.values()))]
        )
    print_table(["model"] + KINDS + ["context-overflow retries", "total"], rows)

    if by_kind_detail:
        print("\nBy compensation and detail:")
        print_table(
            ["kind", "detail", "count"],
            [[kind, detail, n] for (kind, detail), n in by_kind_detail.most_common()],
        )

    if judge_latencies:
        print("\nJudge verdict latency:")
        print_table(
            ["p50", "p90", "max"],
            [[fmt_ms(percentile(judge_latencies, 50)),
              fmt_ms(percentile(judge_latencies, 90)),
              fmt_ms(max(judge_latencies))]],
        )

    print(
        "\nReview discipline: each counter maps to a compensation cake carries "
        "for a model weakness. A counter flatlined at zero for a model makes "
        "that compensation a deletion candidate; review before deleting."
    )


def main() -> None:
    ns = cakelib.build_arg_parser(__doc__).parse_args()
    run(cakelib.load(ns))


if __name__ == "__main__":
    main()
