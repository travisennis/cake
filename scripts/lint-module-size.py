#!/usr/bin/env python3
"""Lint Rust module sizes against configured thresholds.

Checks each .rs file under src/ for:
  - Production lines exceeding threshold (>800 → warn)
  - Test lines exceeding threshold with high ratio (>800 and >40% → warn)

Test module detection uses brace-counting, not regex.
Files matching *_tests.rs are treated as test-only (all lines counted as test).
Exit code is always 0; this is an informational lint.
"""

from __future__ import annotations

import os
import sys

SRC_DIR = "src"
THRESHOLD_PROD = 800
THRESHOLD_TEST = 800
THRESHOLD_RATIO = 0.40


def find_rust_files(src_dir: str) -> list[str]:
    """Return sorted list of .rs files under src_dir."""
    rust_files: list[str] = []
    for root, _dirs, files in os.walk(src_dir):
        for f in files:
            if f.endswith(".rs"):
                rust_files.append(os.path.join(root, f))
    return sorted(rust_files)


def is_test_only_file(filepath: str) -> bool:
    """Return True if the filename indicates it is a test-only module.

    Test-only files extracted via #[path = "..."] mod tests; typically
    use the _tests.rs suffix.
    """
    basename = os.path.basename(filepath)
    return basename.endswith("_tests.rs")


def _extract_mod_name(line: str) -> str | None:
    """If a line declares a `mod <name>` at the top level, return the name.

    Returns None if the line does not appear to be a module declaration.
    Does not parse attributes — just looks for the `mod` keyword followed
    by a Rust identifier. Only matches inline blocks (mod name {),
    not external module declarations (mod name;).
    """
    rest = line.lstrip()
    if not rest.startswith("mod "):
        return None
    # Peel off "mod "
    after_mod = rest[4:].lstrip()
    # Collect the identifier (stop at {, ;, whitespace, etc.)
    name = ""
    for ch in after_mod:
        if ch.isalnum() or ch == "_":
            name += ch
        else:
            break
    if not name:
        return None
    # Only match inline block `mod name {`, not external `mod name;`
    # Check what follows the name
    after_name = after_mod[len(name):].lstrip()
    if after_name.startswith(";"):
        return None
    # Also skip `mod name where name;` pattern (unlikely but safe)
    if "{" not in after_name and ";" in after_name:
        return None
    return name if name else None


def _looks_like_test_mod(name: str) -> bool:
    """Return True if a module name suggests it is a test module.

    Matches names that contain "test" (case-insensitive), which covers
    the common conventions: `tests`, `error_tests`, `my_tests`, etc.
    """
    return "test" in name.lower()


def count_lines(filepath: str) -> tuple[int, int]:
    """Return (prod_lines, test_lines) using brace-counting for test module blocks.

    Test modules are identified by `mod <name>` blocks where the name
    contains "test" (case-insensitive). Brace-counting is used to find
    the matching closing brace, not regex.

    For test-only files (*_tests.rs), all lines are counted as test lines.
    """
    with open(filepath) as f:
        lines = f.readlines()

    # Test-only files: every line is a test line
    if is_test_only_file(filepath):
        return 0, len(lines)

    prod = 0
    test = 0
    brace_depth = 0
    in_test = False
    test_start_depth = 0
    test_pending = False  # True from `mod <test>` until opening `{`

    for line in lines:
        # Detect start of a test module `mod <name> { }` block
        if not in_test and not test_pending:
            mod_name = _extract_mod_name(line)
            if mod_name is not None and _looks_like_test_mod(mod_name):
                test_pending = True
                test_start_depth = brace_depth

        # Count braces on this line
        for ch in line:
            if ch == "{":
                brace_depth += 1
            elif ch == "}":
                brace_depth -= 1

        # If we were in pending test state and now have an opening brace,
        # officially enter test mode.
        if test_pending:
            if brace_depth > test_start_depth:
                in_test = True
                test_pending = False

        # Classify the line
        if in_test or test_pending:
            test += 1
            if in_test and brace_depth == test_start_depth:
                in_test = False
        else:
            prod += 1

    return prod, test


def format_count(n: int, label: str) -> str:
    """Return a human-readable count string."""
    return f"{n} {label} line{'s' if n != 1 else ''}"


def main() -> int:
    """Run the lint and print warnings for files exceeding thresholds."""
    # Resolve src/ relative to the project root (parent of scripts/)
    script_dir = os.path.dirname(os.path.abspath(__file__))
    project_root = os.path.dirname(script_dir)
    src_dir = os.path.join(project_root, SRC_DIR)
    if not os.path.isdir(src_dir):
        print(f"ERROR: source directory not found: {src_dir}", file=sys.stderr)
        return 0

    rust_files = find_rust_files(src_dir)
    any_violations = False

    for filepath in rust_files:
        prod, test = count_lines(filepath)
        total = prod + test
        test_ratio = test / total if total > 0 else 0.0

        issues: list[str] = []

        if prod > THRESHOLD_PROD:
            issues.append(
                f"{format_count(prod, 'production')} (threshold: {THRESHOLD_PROD})"
            )

        if test > THRESHOLD_TEST and test_ratio > THRESHOLD_RATIO:
            ratio_pct = round(test_ratio * 100)
            issues.append(
                f"{format_count(test, 'test')}, "
                f"{ratio_pct}% test ratio "
                f"(threshold: {THRESHOLD_TEST} lines / "
                f"{round(THRESHOLD_RATIO * 100)}%)"
            )

        if issues:
            relpath = os.path.relpath(filepath)
            for issue in issues:
                print(f"{relpath}: {issue}")
            any_violations = True

    if not any_violations:
        print("All module sizes are within thresholds.")

    return 0


if __name__ == "__main__":
    sys.exit(main())
