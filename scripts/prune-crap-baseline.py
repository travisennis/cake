#!/usr/bin/env python3
"""Shrink a cargo-crap JSON report to the complexity grandfather set (ADR 039).

`scripts/check-cc.sh` computes a function's allowed CC as `max(target, baseline
CC)` for a function in the baseline and the target alone for one absent from it.
For an entry at or below the target those are the same ceiling, so such an entry
cannot change a verdict. The committed `ci/cargo-crap-baseline.json` therefore
holds only the entries that can: one `{file, function, cyclomatic}` row per
function whose CC exceeds the target. Dropping `line`, `coverage`, and the
per-entry `crap` value removes the churn and cross-machine noise that made a
regeneration rewrite hundreds of unrelated rows.

A `crap` allowance is carried forward for any `(file, function)` that already has
one in the committed baseline, so the one deliberate CRAP grandfather
(`Skill::parse_frontmatter_fallback`, tracked by the reduction issue) survives a
regeneration instead of being silently dropped. Carrying it forward is also what
makes regenerating on a clean tree a no-op: the output is derived from the fresh
report plus the committed allowances, never from the machine that ran it.

Usage:
  scripts/prune-crap-baseline.py --report report.json --output ci/cargo-crap-baseline.json
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

DEFAULT_TARGET = 10


def load_entries(path: Path | None) -> list[dict]:
    if path is None or not path.is_file():
        return []
    with path.open(encoding="utf-8") as handle:
        return json.load(handle).get("entries", [])


def load_version(path: Path | None) -> str | None:
    if path is None or not path.is_file():
        return None
    with path.open(encoding="utf-8") as handle:
        return json.load(handle).get("version")


def prune(
    report: list[dict],
    baseline: list[dict],
    target: float,
) -> list[dict]:
    # A grandfather is a baseline entry that carries `crap`; its allowance must
    # survive even if the function's CC drops to or below the target.
    allowances: dict[tuple[str, str], float] = {}
    for entry in baseline:
        if "crap" in entry:
            key = (entry["file"], entry["function"])
            allowances[key] = max(allowances.get(key, 0.0), float(entry["crap"]))

    # Multiple report entries may share a name across generic instantiations;
    # take the highest CC, matching scripts/check-cc.sh.
    highest: dict[tuple[str, str], float] = {}
    for entry in report:
        key = (entry["file"], entry["function"])
        highest[key] = max(highest.get(key, 0.0), float(entry["cyclomatic"]))

    entries: list[dict] = []
    for (file, function), cc in sorted(highest.items()):
        if cc <= target and (file, function) not in allowances:
            continue
        entry = {"file": file, "function": function, "cyclomatic": cc}
        if (file, function) in allowances:
            entry["crap"] = allowances[(file, function)]
        entries.append(entry)
    return entries


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--report", type=Path, required=True, help="fresh cargo-crap --format json report")
    parser.add_argument(
        "--baseline",
        type=Path,
        help="committed baseline whose `crap` allowances are carried forward",
    )
    parser.add_argument("--output", type=Path, required=True, help="path to write the pruned baseline")
    parser.add_argument(
        "--target",
        type=float,
        default=DEFAULT_TARGET,
        help=f"CC target; entries at or below it are pruned (default: {DEFAULT_TARGET})",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    ns = build_parser().parse_args(argv)
    if not ns.report.is_file():
        print(f"ERROR: report not found: {ns.report}", file=sys.stderr)
        return 2

    report = load_entries(ns.report)
    baseline = load_entries(ns.baseline)
    version = load_version(ns.report) or load_version(ns.baseline) or "0.0.0"

    entries = prune(report, baseline, ns.target)
    document = {"version": version, "entries": entries}
    ns.output.parent.mkdir(parents=True, exist_ok=True)
    with ns.output.open("w", encoding="utf-8") as handle:
        json.dump(document, handle, indent=2)
        handle.write("\n")

    grandfathers = sum(1 for entry in entries if "crap" in entry)
    print(f"Pruned baseline: {len(entries)} entries ({grandfathers} CRAP grandfathers) -> {ns.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
