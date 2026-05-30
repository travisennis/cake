#!/usr/bin/env python3
"""Check that generated `.agents/` index files are consistent with source files.

Replaces the previous `ahm --dry-run index` check that required the external
`ahm` tool. This script validates that every source file in the task, research,
and exec-plan directories has a corresponding entry in the generated index,
without needing to regenerate the indexes themselves.

Exit code:
  0 – indexes are current
  1 – at least one index is stale or missing an entry

Usage:
    python3 scripts/check-indexes.py [--verbose]
"""

from __future__ import annotations

import argparse
import os
import re
import sys

PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
AGENTS_DIR = os.path.join(PROJECT_ROOT, ".agents")


def warn(msg: str) -> None:
    print(f"ERROR: {msg}", file=sys.stderr)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def read_file(path: str) -> str:
    """Read a file, returning empty string if it doesn't exist."""
    try:
        with open(path, encoding="utf-8") as fh:
            return fh.read()
    except FileNotFoundError:
        return ""


def extract_id_from_front_matter(content: str) -> str | None:
    """Extract the `id:` field from a YAML front-matter block."""
    m = re.search(
        r"^---\s*\n(.*?)\n---", content, re.DOTALL
    )
    if not m:
        return None
    front = m.group(1)
    for line in front.splitlines():
        line = line.strip()
        if line.startswith("id:"):
            val = line[3:].strip()
            if val:
                return val
    return None


def md_files_in(dirpath: str, exclude: set[str] | None = None) -> list[str]:
    """Return sorted list of .md filenames (not full paths) under *dirpath*.

    Skips files in *exclude* (default: ``{"index.md"}``).
    """
    if exclude is None:
        exclude = {"index.md"}
    try:
        names = sorted(
            f
            for f in os.listdir(dirpath)
            if f.endswith(".md") and f not in exclude
        )
    except FileNotFoundError:
        return []
    return names


# ---------------------------------------------------------------------------
# Task index check
# ---------------------------------------------------------------------------

def _check_orphaned_task_entries(
    index_text: str, src_dir: str, index_label: str, verbose: bool
) -> int:
    """Check that every `[<id>](<filename>)` link in the index has a
    corresponding source file."""
    failures = 0
    for m in re.finditer(r"\[(\d+[a-z]?)\]\(([^)]+\.md)\)", index_text):
        task_id = m.group(1)
        fname = m.group(2)
        fpath = os.path.join(src_dir, fname)
        if not os.path.isfile(fpath):
            warn(
                f"{index_label} index references `{fname}` "
                f"(id={task_id}) but file does not exist"
            )
            failures += 1
            continue
        if verbose:
            print(f"  ✓ {fname} (id={task_id}) source file exists")
    return failures


def check_task_dir(
    rel_dir: str, label: str, verbose: bool
) -> int:
    """Check that every `.md` source in ``.agents/.tasks/<rel_dir>/`` has an
    entry in the corresponding ``index.md``, and vice versa.

    Returns 0 if OK, 1 on failure.
    """
    src_dir = os.path.join(AGENTS_DIR, ".tasks", rel_dir)
    index_path = os.path.join(src_dir, "index.md")

    if not os.path.isdir(src_dir):
        return 0  # directory doesn't exist – nothing to check (yet)

    index_text = read_file(index_path)
    if not index_text:
        warn(f"{index_path} is missing or empty")
        return 1

    failures = 0

    # Forward check: every source file has an index entry
    for fname in md_files_in(src_dir):
        fpath = os.path.join(src_dir, fname)
        content = read_file(fpath)
        task_id = extract_id_from_front_matter(content)
        if task_id is None:
            warn(f"{fpath}: could not extract `id:` from front matter")
            failures += 1
            continue

        # Look for [<id>](<fname>) in the index
        pattern = re.escape(f"[{task_id}]({fname})")
        if not re.search(pattern, index_text):
            warn(
                f"{index_path}: missing entry for `{fname}` "
                f"(id={task_id})"
            )
            failures += 1
            continue

        if verbose:
            print(f"  ✓ {fname} (id={task_id}) found in {label} index")

    # Reverse check: every index entry has a source file
    failures += _check_orphaned_task_entries(
        index_text, src_dir, label, verbose
    )

    return 1 if failures else 0


def check_master_task_index(verbose: bool) -> int:
    """Check that ``.agents/.tasks/index.md`` contains entries for every task
    across active, completed, and cancelled directories."""
    master_path = os.path.join(AGENTS_DIR, ".tasks", "index.md")
    master_text = read_file(master_path)
    if not master_text:
        warn(f"{master_path} is missing or empty")
        return 1

    failures = 0
    for rel_dir in ("active", "completed", "cancelled"):
        src_dir = os.path.join(AGENTS_DIR, ".tasks", rel_dir)
        if not os.path.isdir(src_dir):
            continue
        for fname in md_files_in(src_dir):
            fpath = os.path.join(src_dir, fname)
            content = read_file(fpath)
            task_id = extract_id_from_front_matter(content)
            if task_id is None:
                continue  # already reported in check_task_dir

            # Check for the id anywhere in the master index (the link target
            # in the master index is relative to `.tasks/` e.g. `active/004.md`)
            # Pattern: [<id>](<rel_dir>/<fname>)
            rel_link = f"[{task_id}]({rel_dir}/{fname})"
            if rel_link not in master_text:
                warn(
                    f"{master_path}: missing entry for `{rel_dir}/{fname}` "
                    f"(id={task_id})"
                )
                failures += 1
                continue

            if verbose:
                print(f"  ✓ {rel_dir}/{fname} (id={task_id}) found in master index")

    return 1 if failures else 0


# ---------------------------------------------------------------------------
# Research index check
# ---------------------------------------------------------------------------

def check_research_index(verbose: bool) -> int:
    """Check that every research note appears in ``.agents/.research/index.md``."""
    research_dir = os.path.join(AGENTS_DIR, ".research")
    index_path = os.path.join(research_dir, "index.md")
    index_text = read_file(index_path)
    if not index_text:
        warn(f"{index_path} is missing or empty")
        return 1

    failures = 0
    # Research notes are organised by subdirectory category
    for category in ("inbox", "investigations", "sources", "topics", "archived"):
        cat_dir = os.path.join(research_dir, category)
        if not os.path.isdir(cat_dir):
            continue
        for fname in md_files_in(cat_dir, exclude=set()):
            full_rel = f"{category}/{fname}"
            # The index links are of the form [title](category/fname.md)
            if f"({full_rel})" not in index_text:
                warn(f"{index_path}: missing entry for `{full_rel}`")
                failures += 1
                continue
            if verbose:
                print(f"  ✓ {full_rel} found in research index")

    return 1 if failures else 0


# ---------------------------------------------------------------------------
# ExecPlan index check
# ---------------------------------------------------------------------------

def check_execplan_dir(rel_dir: str, label: str, verbose: bool) -> int:
    """Check exec-plan index for *rel_dir* (``active`` or ``completed``)."""
    ep_dir = os.path.join(AGENTS_DIR, "exec-plans", rel_dir)
    index_path = os.path.join(ep_dir, "index.md")
    index_text = read_file(index_path)
    if not index_text:
        warn(f"{index_path} is missing or empty")
        return 1

    failures = 0
    for fname in md_files_in(ep_dir):
        # The index links are of the form [title](fname)
        if f"({fname})" not in index_text:
            warn(f"{index_path}: missing entry for `{fname}`")
            failures += 1
            continue
        if verbose:
            print(f"  ✓ {fname} found in {label} exec-plan index")

    return 1 if failures else 0


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(
        description="Check that generated .agents/ index files are current"
    )
    parser.add_argument(
        "--verbose", "-v",
        action="store_true",
        help="Print per-file results",
    )
    args = parser.parse_args()

    if not os.path.isdir(AGENTS_DIR):
        print(f"{AGENTS_DIR} not found – nothing to check", file=sys.stderr)
        return 0

    total_failures = 0

    # Task indexes
    if args.verbose:
        print("Checking task indexes...")
    for rel_dir in ("active", "completed", "cancelled"):
        total_failures += check_task_dir(rel_dir, rel_dir, args.verbose)
    total_failures += check_master_task_index(args.verbose)

    # Research index
    if args.verbose:
        print("Checking research index...")
    total_failures += check_research_index(args.verbose)

    # ExecPlan indexes
    if args.verbose:
        print("Checking exec-plan indexes...")
    for rel_dir, label in (("active", "active"), ("completed", "completed")):
        total_failures += check_execplan_dir(rel_dir, label, args.verbose)

    if total_failures:
        print(
            "\nTask indexes are stale. "
            "Regenerate with `ahm index`.",
            file=sys.stderr,
        )
        return 1

    if args.verbose:
        print("\nAll indexes are current.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
