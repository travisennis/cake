#!/usr/bin/env python3
"""Lint Rust module sizes against the production-line threshold.

Checks each .rs file under src/ and warns when it exceeds 800 lines.
Exit code is always 0; this is an informational lint.

Known limitation: the count is a raw line count, so blank lines and comments
count. Treat the report as a direction, not a gate.
"""

from __future__ import annotations

import os
import sys

SRC_DIR = "src"
THRESHOLD_PROD = 800


def find_rust_files(src_dir: str) -> list[str]:
    """Return sorted list of .rs files under src_dir."""
    rust_files: list[str] = []
    for root, _dirs, files in os.walk(src_dir):
        for f in files:
            if f.endswith(".rs"):
                rust_files.append(os.path.join(root, f))
    return sorted(rust_files)


def count_lines(filepath: str) -> int:
    """Return the number of lines in filepath."""
    with open(filepath) as f:
        return len(f.readlines())


def main() -> int:
    """Run the lint and print warnings for files exceeding the threshold."""
    # Resolve src/ relative to the project root (parent of scripts/)
    script_dir = os.path.dirname(os.path.abspath(__file__))
    project_root = os.path.dirname(script_dir)
    src_dir = os.path.join(project_root, SRC_DIR)
    if not os.path.isdir(src_dir):
        print(f"ERROR: source directory not found: {src_dir}", file=sys.stderr)
        return 0

    any_violations = False
    for filepath in find_rust_files(src_dir):
        lines = count_lines(filepath)
        if lines > THRESHOLD_PROD:
            relpath = os.path.relpath(filepath)
            print(f"{relpath}: {lines} production lines (threshold: {THRESHOLD_PROD})")
            any_violations = True

    if not any_violations:
        print("All module sizes are within thresholds.")

    return 0


if __name__ == "__main__":
    sys.exit(main())
