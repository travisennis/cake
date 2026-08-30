#!/usr/bin/env python3
"""API reliability: request latency, status codes, errors, retries, context overflow.

Source: telemetry api_attempt and retry_scheduled records.
"""

from collections import Counter

import cakelib
from cakelib import fmt_int, fmt_ms, fmt_pct, percentile, print_header, print_table


def _retry_reason_rows(retries: list[dict]) -> list[dict]:
    """Per-reason retry aggregates: count, delays, depth, cap, override changes."""
    reasons = Counter(r.get("reason", "?") for r in retries)
    delay_by_reason = cakelib.group_by(retries, lambda r: r.get("reason", "?"))
    rows = []
    for reason, n in reasons.most_common():
        group = delay_by_reason[reason]
        delays = [r["delay_ms"] for r in group]
        rows.append({
            "reason": reason,
            "count": n,
            "delay_p50": percentile(delays, 50),
            "delay_max": max(delays),
            "deepest": max(r.get("attempt", 1) for r in group),
            "max_retries": max(r.get("max_retries", 0) for r in group),
            "changed_overrides": sum(1 for r in group if r.get("changed_request_overrides")),
        })
    return rows


def aggregate(data: cakelib.Dataset) -> dict:
    """Collate API attempt and retry aggregates used by the report.

    Returns a dict of:
    - attempts: list of (invocation, attempt) pairs
    - terminations: Counter(classification -> attempts)
    - retry_reasons: per-reason list of dicts from _retry_reason_rows
    """
    attempts = [(inv, a) for inv in data.invocations for a in inv.attempts]
    retries = [r for inv in data.invocations for r in inv.retries]
    return {
        "attempts": attempts,
        "terminations": Counter(
            (a.get("termination") or {}).get("classification", "unknown")
            for _, a in attempts
        ),
        "retry_reasons": _retry_reason_rows(retries),
    }


def run(data: cakelib.Dataset) -> None:
    print_header("API RELIABILITY")
    print(cakelib.describe_window(data))

    agg = aggregate(data)
    attempts = agg["attempts"]
    if not attempts:
        print("\nNo telemetry api_attempt records in window.")
        return

    failures = [(inv, a) for inv, a in attempts if a.get("error") or a.get("status_code") not in (200, None)]
    print(f"\nAttempts: {fmt_int(len(attempts))} | failed: {fmt_int(len(failures))} "
          f"({fmt_pct(len(failures), len(attempts))})")

    print("\nStatus codes:")
    codes = Counter(a.get("status_code") for _, a in attempts)
    print_table(
        ["status", "attempts"],
        [[c if c is not None else "(none)", n] for c, n in codes.most_common()],
    )

    if failures:
        print("\nTop error variants:")
        errors = Counter((a.get("error") or "?").splitlines()[0][:90] for _, a in failures)
        print_table(["error", "count"], [[e, n] for e, n in errors.most_common(10)])

    print("\nTermination classification:")
    print_table(
        ["classification", "attempts"],
        [[c, fmt_int(n)] for c, n in agg["terminations"].most_common()],
    )

    print("\nLatency by model (request_ms):")
    rows = []
    by_model = cakelib.group_by(attempts, lambda ia: ia[0].model)
    for model, group in sorted(by_model.items(), key=lambda kv: -len(kv[1])):
        lat = [a["request_ms"] for _, a in group]
        rows.append([model, fmt_int(len(group)), fmt_ms(percentile(lat, 50)),
                     fmt_ms(percentile(lat, 90)), fmt_ms(percentile(lat, 99)), fmt_ms(max(lat))])
    print_table(["model", "attempts", "p50", "p90", "p99", "max"], rows)

    retries = [r for inv in data.invocations for r in inv.retries]
    print(f"\nRetries scheduled: {fmt_int(len(retries))} "
          f"({fmt_pct(len(retries), len(attempts))} of attempts)")
    if agg["retry_reasons"]:
        print("\nRetry reasons:")
        rows = [
            [row["reason"], row["count"], fmt_ms(row["delay_p50"]), fmt_ms(row["delay_max"]),
             row["deepest"], row["max_retries"], row["changed_overrides"]]
            for row in agg["retry_reasons"]
        ]
        print_table(["reason", "count", "delay p50", "delay max", "deepest attempt",
                     "max retries", "changed overrides"], rows)

    overflow = sum(
        1 for _, a in attempts
        if (a.get("request_overrides") or {}).get("context_overflow_retry_used")
    )
    if overflow:
        print(f"\nAttempts sent with context-overflow override active: {fmt_int(overflow)}")

    print("\nAttempts per invocation (proxy for turns):")
    per_inv = [len(inv.attempts) for inv in data.invocations if inv.attempts]
    print_table(["p50", "p90", "max"], [[
        fmt_int(percentile(per_inv, 50)), fmt_int(percentile(per_inv, 90)), fmt_int(max(per_inv)),
    ]])


def main() -> None:
    ns = cakelib.build_arg_parser(__doc__).parse_args()
    run(cakelib.load(ns))


if __name__ == "__main__":
    main()
