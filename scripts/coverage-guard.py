#!/usr/bin/env python3
"""Reject coverage runs whose profile data cannot be trusted (#520).

Two conditions have reported false coverage totals:

- Profile data that survived the coverage clean merges into the next run. No clean
  mode removes everything: `cargo llvm-cov clean --profraw-only` leaves the merged
  `.profdata` behind, and `--workspace` leaves the previous run's `-profraw-list`.
  So the gate first removes every residual profile artifact itself (the documented
  policy) and then reruns this guard to prove the directory is clean, refusing to
  evaluate the threshold if anything survived both steps.
- One source file reported under two spellings of the same path (a checkout
  reached through a symlinked prefix, for example) splits its coverage across
  two entries and understates the total.

`scripts/check-coverage.sh` runs this guard around the instrumented run and stops
before evaluating the threshold when either condition is present, so a misleading
number cannot be mistaken for a verdict. Exit code 1 means the coverage data is
untrustworthy, 2 means the invocation was wrong, and 0 means the data passed the
guard.

Usage:
  scripts/coverage-guard.py --profiles-dir target/llvm-cov-target --remove-residual
  scripts/coverage-guard.py --profiles-dir target/llvm-cov-target
  scripts/coverage-guard.py --lcov lcov.info
"""

from __future__ import annotations

import argparse
import os
import sys
from collections import defaultdict
from pathlib import Path

# `cargo llvm-cov clean --workspace` is the documented recovery: it removes the
# artifacts that may affect coverage results, where `--profraw-only` leaves the
# merged profile data behind.
RECOVERY = "Recovery: run `cargo llvm-cov clean --workspace`, remove any files listed above, then rerun the gate (`just check-coverage`)."

PROFILE_SUFFIXES = (".profraw", ".profdata")
PROFILE_LIST_SUFFIX = "-profraw-list"

MAX_REPORTED = 10


def residual_profiles(directory: Path) -> list[Path]:
    """Profile artifacts still present in the coverage target directory."""
    if not directory.is_dir():
        return []
    return sorted(
        path
        for path in directory.rglob("*")
        if path.is_file()
        and (path.name.endswith(PROFILE_SUFFIXES) or path.name.endswith(PROFILE_LIST_SUFFIX))
    )


def lcov_sources(path: Path) -> list[str]:
    """Source paths recorded in an LCOV file, in file order."""
    sources: list[str] = []
    with path.open(encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if line.startswith("SF:"):
                sources.append(line[len("SF:") :].rstrip("\n"))
    return sources


def duplicate_roots(sources: list[str]) -> dict[str, list[str]]:
    """Files recorded under more than one spelling, keyed by canonical path."""
    spellings: dict[str, set[str]] = defaultdict(set)
    for source in sources:
        spellings[os.path.realpath(source)].add(source)
    return {real: sorted(names) for real, names in spellings.items() if len(names) > 1}


def remove_residual(residual: list[Path]) -> list[Path]:
    """Remove residual profile artifacts, returning those actually removed."""
    removed: list[Path] = []
    for path in residual:
        try:
            path.unlink()
        except OSError as error:
            print(f"ERROR: could not remove {path}: {error}", file=sys.stderr)
            continue
        removed.append(path)
    return removed


def report_residual(directory: Path, residual: list[Path]) -> None:
    print(
        f"ERROR: {len(residual)} profile artifact(s) survived the coverage clean in {directory}:",
        file=sys.stderr,
    )
    for path in residual[:MAX_REPORTED]:
        print(f"  {path}", file=sys.stderr)
    if len(residual) > MAX_REPORTED:
        print(f"  ... and {len(residual) - MAX_REPORTED} more", file=sys.stderr)
    print(
        "Stale profile data merges into this run and reports a false total, so the "
        "coverage threshold is not evaluated.",
        file=sys.stderr,
    )
    print(RECOVERY, file=sys.stderr)


def report_duplicate_roots(duplicates: dict[str, list[str]]) -> None:
    print(
        "ERROR: the coverage report contains duplicate source roots: "
        f"{len(duplicates)} file(s) appear under more than one spelling of their path:",
        file=sys.stderr,
    )
    for real, names in sorted(duplicates.items())[:MAX_REPORTED]:
        print(f"  {real}", file=sys.stderr)
        for name in names:
            print(f"    recorded as: {name}", file=sys.stderr)
    if len(duplicates) > MAX_REPORTED:
        print(f"  ... and {len(duplicates) - MAX_REPORTED} more", file=sys.stderr)
    print(
        "Coverage splits across both spellings and understates the total, so the "
        "coverage threshold is not evaluated.",
        file=sys.stderr,
    )
    print(RECOVERY, file=sys.stderr)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--profiles-dir",
        type=Path,
        help="coverage target directory to scan for residual profile data",
    )
    parser.add_argument(
        "--lcov",
        type=Path,
        help="LCOV file whose recorded source paths must resolve to one root",
    )
    parser.add_argument(
        "--remove-residual",
        action="store_true",
        help="remove residual profile artifacts instead of failing on them",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    ns = parser.parse_args(argv)
    if (ns.profiles_dir is None) == (ns.lcov is None):
        parser.error("exactly one of --profiles-dir or --lcov is required")
    if ns.remove_residual and ns.profiles_dir is None:
        parser.error("--remove-residual applies to --profiles-dir")

    if ns.profiles_dir is not None:
        residual = residual_profiles(ns.profiles_dir)
        if ns.remove_residual:
            removed = remove_residual(residual)
            if len(removed) != len(residual):
                report_residual(ns.profiles_dir, [path for path in residual if path not in removed])
                return 1
            for path in removed:
                print(f"Removed residual profile artifact: {path}")
            if not removed:
                print(f"Coverage artifact guard: no residual profile data in {ns.profiles_dir}")
            return 0
        if residual:
            report_residual(ns.profiles_dir, residual)
            return 1
        print(f"Coverage artifact guard: no residual profile data in {ns.profiles_dir}")
        return 0

    if not ns.lcov.is_file():
        print(f"ERROR: lcov file not found: {ns.lcov}", file=sys.stderr)
        return 2
    sources = lcov_sources(ns.lcov)
    duplicates = duplicate_roots(sources)
    if duplicates:
        report_duplicate_roots(duplicates)
        return 1
    print(f"Coverage artifact guard: {len(sources)} source file(s) under one root")
    return 0


if __name__ == "__main__":
    sys.exit(main())
