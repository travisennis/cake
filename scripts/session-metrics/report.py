#!/usr/bin/env python3
"""Combined report: runs every metric module against a single load of the data.

Usage:
  python3 report.py                 # last 30 days
  python3 report.py --days 0        # all time
  python3 report.py --model deepseek --project cake
  python3 report.py --json --days 7 # one machine-readable summary document
  python3 report.py --baseline FILE # compare against a saved summary
  python3 report.py --vs 7          # last 7 days vs the preceding 7 days
"""

import argparse
import json
from datetime import datetime, timedelta, timezone
from pathlib import Path

import api
import cache_breaks
import cakelib
import compensations
import gates
import hooks
import judge
import outcomes
import overview
import summary
import time_breakdown
import tokens
import tools

SECTIONS = (
    overview, tokens, cache_breaks, tools, api, gates, judge,
    time_breakdown, outcomes, hooks, compensations,
)


def parse_args() -> argparse.Namespace:
    parser = cakelib.build_arg_parser(__doc__)
    comparison = parser.add_mutually_exclusive_group()
    comparison.add_argument(
        "--baseline", type=Path, metavar="FILE",
        help="Compare the current window against a summary document saved from --json.")
    comparison.add_argument(
        "--vs", type=int, metavar="DAYS",
        help="Compare the last DAYS against the immediately preceding DAYS.")
    parser.add_argument(
        "--json", action="store_true",
        help="Print one machine-readable summary document instead of the human report.")
    ns = parser.parse_args()
    if ns.json and (ns.baseline is not None or ns.vs is not None):
        parser.error("--json cannot be combined with --baseline or --vs")
    if ns.vs is not None and ns.vs < 1:
        parser.error("--vs must be a positive number of days")
    return ns


def main() -> None:
    ns = parse_args()
    if ns.json:
        json_report(ns)
    elif ns.baseline is not None:
        baseline_report(ns)
    elif ns.vs is not None:
        vs_report(ns)
    else:
        data = cakelib.load(ns)
        for module in SECTIONS:
            module.run(data)
        print()


def json_report(ns: argparse.Namespace) -> None:
    data = _load_uncapped(ns)
    start, end = _current_window(ns)
    document = summary.summarize(
        data, start, end, measured=end, filters=_filters(ns)
    )
    print(json.dumps(document, indent=2))


def baseline_report(ns: argparse.Namespace) -> None:
    data = _load_uncapped(ns)
    start, end = _current_window(ns)
    current = summary.summarize(
        data, start, end, measured=end, filters=_filters(ns)
    )
    _print_comparison(
        f"Baseline comparison: {ns.baseline}", current, _read_baseline(ns.baseline), "baseline"
    )


def vs_report(ns: argparse.Namespace) -> None:
    data, end = _load_uncapped(ns), _now()
    current_start = end - timedelta(days=ns.vs)
    previous_start = current_start - timedelta(days=ns.vs)
    filters = _filters(ns)
    current = summary.summarize(data, current_start, end, measured=end, filters=filters)
    previous = summary.summarize(
        data, previous_start, current_start, measured=end, filters=filters
    )
    _print_comparison(
        f"Window comparison: last {ns.vs} days vs preceding {ns.vs} days",
        current, previous, "previous",
    )


def _load_uncapped(ns: argparse.Namespace) -> cakelib.Dataset:
    # One load serves both windows; the summary applies explicit bounds.
    return cakelib.load(argparse.Namespace(**{**vars(ns), "days": 0}))


def _current_window(ns: argparse.Namespace) -> tuple[datetime | None, datetime]:
    end = _now()
    return end - timedelta(days=ns.days) if ns.days and ns.days > 0 else None, end


def _now() -> datetime:
    # Second precision keeps the printed bounds identical to the applied ones.
    return datetime.now(timezone.utc).replace(microsecond=0)


def _filters(ns: argparse.Namespace) -> dict:
    return {"model": ns.model, "project": ns.project,
            "session": ns.session, "invocation": ns.invocation}


def _read_baseline(path: Path) -> dict:
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise SystemExit(f"cannot read baseline {path}: {error}") from error
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SystemExit(f"baseline {path} is not valid UTF-8 JSON: {error}") from error
    if not isinstance(document, dict):
        raise SystemExit(f"baseline {path} is not a JSON object")
    if document.get("schema_version") != summary.SCHEMA_VERSION:
        raise SystemExit(
            f"baseline {path} has schema_version {document.get('schema_version')!r}; "
            f"expected {summary.SCHEMA_VERSION}"
        )
    return document


def _print_comparison(title: str, current: dict, reference: dict, label: str) -> None:
    print(title)
    print(f"  current:  {_describe_document(current)}")
    print(f"  {label + ':':<9} {_describe_document(reference)}\n")
    rows = []
    for row in summary.compare(current, reference):
        metric = summary.METRIC_BY_KEY[row["key"]]
        rows.append([row["key"], _fmt_value(metric, row["value"]),
                     _fmt_value(metric, row["baseline"]),
                     _fmt_delta(metric, row["delta"]), row["direction"]])
    cakelib.print_table(["metric", "value", label, "delta", "direction"], rows)
    absent = summary.missing_keys(reference)
    if absent:
        print(f"\n  {label} lacks {len(absent)} documented key(s): {', '.join(absent)}")
    print("\n  Direction is better/worse from the metric's higher_is_better; "
          "n/a means either side is undefined.")


def _describe_document(document: dict) -> str:
    window = document.get("window") or {}
    text = (f"{window.get('start') or 'all time'} to {window.get('end') or 'unbounded'}"
            f" | measured {document.get('measured') or '?'}")
    filters = {key: value for key, value in (document.get("filters") or {}).items() if value}
    return text + (" | filters " + ", ".join(f"{k}={v}" for k, v in filters.items())
                   if filters else "")


def _fmt_value(metric: summary.Metric, value: float | None) -> str:
    if value is None:
        return "-"
    return f"{100 * value:.2f}%" if metric.unit == "rate" else cakelib.fmt_ms(value)


def _fmt_delta(metric: summary.Metric, delta: float | None) -> str:
    if delta is None:
        return "-"
    if metric.unit == "rate":
        return f"{100 * delta:+.3f}pp"
    return ("+" if delta > 0 else "-" if delta < 0 else "") + cakelib.fmt_ms(abs(delta))


if __name__ == "__main__":
    main()
