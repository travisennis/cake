#!/usr/bin/env python3
"""Controlled model evaluation harness (contributor tooling; not a cake CLI
contract). Runs committed fixture tasks across selected models, judges the
resulting repository state with deterministic verifier commands, and writes
stable aggregate JSON plus a human summary.

Usage:
  python3 run_eval.py --list-cases
  python3 run_eval.py --model <name> [--model <name> ...] [--repetitions N]
                      [--cases <name> ...] [--tags <tag> ...] [--cake PATH]
                      [--results-dir DIR] [--verbose]

Real-model runs require configured credentials and authorized external spend;
see README.md in this directory.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

import eval_lib
from eval_lib import (
    OUTCOME_CORRECT,
    HarnessError,
    discover_cases,
    run_trial,
    summarize,
)

DEFAULT_CASES_DIR = Path(__file__).resolve().parent / "cases"
DEFAULT_RESULTS_DIR = Path(__file__).resolve().parent / "results"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="run_eval.py",
        description=(
            "Controlled model evaluation harness (contributor tooling; "
            "not a cake CLI contract)."
        ),
    )
    parser.add_argument(
        "--model", action="append", default=[], metavar="NAME",
        help="model name from settings.toml to evaluate (repeatable; required)",
    )
    parser.add_argument(
        "--repetitions", type=int, default=1, metavar="N",
        help="run each case N times per model (default 1)",
    )
    parser.add_argument(
        "--cases-dir", action="append", type=Path, default=[], metavar="DIR",
        help="directory of fixture cases (repeatable; default: scripts/evals/cases)",
    )
    parser.add_argument(
        "--cases", action="append", default=[], metavar="NAME",
        help="run only the named cases (repeatable; default: all)",
    )
    parser.add_argument(
        "--tags", action="append", default=[], metavar="TAG",
        help="run only cases carrying any of these tags (repeatable)",
    )
    parser.add_argument(
        "--cake", default="cake", metavar="PATH",
        help="cake executable to invoke (default: cake on PATH)",
    )
    parser.add_argument(
        "--results-dir", type=Path, default=DEFAULT_RESULTS_DIR, metavar="DIR",
        help=(
            "directory for generated JSON results, sessions, and telemetry "
            "(default: scripts/evals/results)"
        ),
    )
    parser.add_argument(
        "--list-cases", action="store_true",
        help="list available cases and exit without running anything",
    )
    parser.add_argument(
        "--verbose", action="store_true",
        help="print one line per trial to stderr",
    )
    return parser


def resolve_cake(value: str) -> str:
    """Resolve a path-like cake executable against the invocation directory.

    A bare name (no path separator) is left alone for PATH lookup, matching
    the default `cake`; anything path-like is resolved to an absolute path
    because every trial runs cake with a different working directory.
    """
    if os.sep in value or (os.altsep and os.altsep in value):
        return str(Path(value).resolve())
    return value


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    ns = parser.parse_args(argv)

    try:
        case_dirs = ns.cases_dir or [DEFAULT_CASES_DIR]
        cases = discover_cases(case_dirs)
        if ns.cases:
            known = {case.name for case in cases}
            unknown = [name for name in ns.cases if name not in known]
            if unknown:
                raise HarnessError(f"unknown case(s): {', '.join(unknown)}")
            cases = [case for case in cases if case.name in ns.cases]
        if ns.tags:
            cases = [case for case in cases if set(case.tags) & set(ns.tags)]
    except HarnessError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1

    if ns.list_cases:
        print_cases(cases)
        return 0

    if not ns.model:
        parser.error("at least one --model is required")
    if ns.repetitions < 1:
        parser.error("--repetitions must be at least 1")
    if not cases:
        print("ERROR: no cases selected (use --cases or --tags to filter)", file=sys.stderr)
        return 1

    ns.results_dir.mkdir(parents=True, exist_ok=True)
    cake = resolve_cake(ns.cake)
    trials = []
    for model in ns.model:
        for case in cases:
            for repetition in range(ns.repetitions):
                trial = run_trial(case, model, repetition, [cake], ns.results_dir)
                trials.append(trial)
                if ns.verbose:
                    print(trial_line(trial), file=sys.stderr)

    summary = summarize(trials)
    print_summary(summary)
    result = build_result(ns, cases, trials, summary)
    result_path = write_results(ns.results_dir, result)
    print(f"\nResults written to: {result_path}")
    print(f"Results directory: {ns.results_dir}")
    return 0


def print_cases(cases: list) -> None:
    for case in cases:
        tags = ", ".join(case.tags)
        print(f"{case.name}  [{tags}]")
        if case.description:
            print(f"    {case.description}")


def trial_line(trial: dict) -> str:
    turns = trial["turns"] if trial["turns"] is not None else "-"
    tool_calls = trial["tool_calls"] if trial["tool_calls"] is not None else "-"
    return (
        f"{trial['model']} {trial['case']} rep {trial['repetition']}: "
        f"{trial['outcome']} ({trial['duration_ms']}ms, {turns} turns, "
        f"{tool_calls} tool calls)"
    )


def print_summary(summary: dict) -> None:
    print("\nEvaluation summary")
    print_stats_table("by model", summary["by_model"])
    print_stats_table("by case tag", summary["by_tag"])


def print_stats_table(title: str, groups: dict) -> None:
    headers = [
        "group", "trials", "correct", "incorrect", "cake", "provider",
        "timeout", "harness", "rate", "turns p50/p90", "tokens p50/p90",
        "duration p50/p90", "tool fail p50/p90",
    ]
    rows = []
    for group, stats in groups.items():
        counts = stats["outcomes"]
        rows.append([
            group,
            str(stats["trials"]),
            str(counts.get("correct", 0)),
            str(counts.get("incorrect", 0)),
            str(counts.get("cake_error", 0)),
            str(counts.get("provider_error", 0)),
            str(counts.get("timeout", 0)),
            str(counts.get("harness_error", 0)),
            pct(stats["correctness_rate"]),
            pair(stats["median_turns"], stats["p90_turns"]),
            pair(stats["median_tokens"], stats["p90_tokens"]),
            pair_ms(stats["median_duration_ms"], stats["p90_duration_ms"]),
            pair(stats["median_tool_failures"], stats["p90_tool_failures"]),
        ])
    print(f"\n{title}:")
    print_table(headers, rows)


def pair(median, p90) -> str:
    def fmt(value):
        return "-" if value is None else f"{value}"
    return f"{fmt(median)}/{fmt(p90)}"


def pair_ms(median, p90) -> str:
    def fmt(value):
        if value is None:
            return "-"
        return f"{value / 1000:.1f}s" if value >= 1000 else f"{value:.0f}ms"
    return f"{fmt(median)}/{fmt(p90)}"


def pct(rate) -> str:
    return f"{rate * 100:.1f}%" if rate is not None else "-"


def print_table(headers: list[str], rows: list[list[str]], indent: int = 2) -> None:
    if not rows:
        print(" " * indent + "(no data)")
        return
    widths = [len(header) for header in headers]
    for row in rows:
        for i, cell in enumerate(row):
            widths[i] = max(widths[i], len(str(cell)))
    pad = " " * indent

    def line(cells):
        return pad + "  ".join(str(c).ljust(widths[i]) for i, c in enumerate(cells)).rstrip()

    print(line(headers))
    print(pad + "  ".join("-" * w for w in widths))
    for row in rows:
        print(line(row))


def build_result(ns: argparse.Namespace, cases: list, trials: list, summary: dict) -> dict:
    return {
        "schema_version": eval_lib.SCHEMA_VERSION,
        "tool": {"name": eval_lib.TOOL_NAME, "version": eval_lib.TOOL_VERSION},
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "configuration": {
            "cake_command": [resolve_cake(ns.cake)],
            "models": list(ns.model),
            "repetitions": ns.repetitions,
            "cases": [case.name for case in cases],
            "results_dir": str(ns.results_dir),
        },
        "trials": trials,
        "summary": summary,
    }


def write_results(results_dir: Path, result: dict) -> Path:
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S-%f")
    serialized = json.dumps(result, indent=2) + "\n"
    path = results_dir / f"run-{timestamp}.json"
    path.write_text(serialized)
    (results_dir / "latest.json").write_text(serialized)
    return path


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        # Match cake's INTERRUPTED exit code; in-flight trials have already
        # terminated their cake subprocess groups (see eval_lib.run_trial).
        sys.exit(130)
