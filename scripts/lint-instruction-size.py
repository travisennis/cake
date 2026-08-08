#!/usr/bin/env python3
"""Enforce word budgets on the agent instruction corpus.

The instruction corpus is the prose an agent loads to decide how to work:
AGENTS.md, CONTRIBUTING.md, ARCHITECTURE.md, the topic and workflow documents
under docs/, the runbooks, and the skills. Records are excluded --- ADRs,
ExecPlans, and research notes are history rather than instructions, and
ExecPlans are required to be self-contained, so a length budget would fight
their purpose.

Counting is prose-only: fenced code blocks are stripped before counting, so a
document is not penalised for carrying the commands a reader needs.

A document over its budget is a signal that adding guidance should displace
guidance, not accumulate on top of it. ALLOWANCES grandfathers documents that
were already over budget when this check landed; those may shrink but never
grow, and the entry is deleted once the document reaches its budget.

Exit code is 1 when any document exceeds its budget or allowance.
"""

from __future__ import annotations

import os
import sys

# Directories walked recursively for Markdown instructions, relative to the
# repo root. Excluded by omission: docs/adr and docs/exec-plans hold records
# rather than instructions.
INSTRUCTION_DIRS = [
    "docs/guardrails",
    "docs/workflow",
    "docs/runbooks",
    "docs/automations",
    ".agents/skills",
]

# Directories whose top-level Markdown files are instructions, not walked into.
# docs/ is scanned this way so that a new topic document is covered by the
# budget the day it lands, without pulling in the record subdirectories.
INSTRUCTION_DIRS_SHALLOW = [
    "docs",
]

# Individual instruction files relative to the repo root.
INSTRUCTION_FILES = [
    "AGENTS.md",
    "CONTRIBUTING.md",
    "ARCHITECTURE.md",
]

# Per-document budgets in prose words. AGENTS.md is the routing document every
# session pays for, so it carries the tightest budget.
#
# Like the allowances below, these carry headroom above the document's current
# size rather than pinning it. A budget set to the exact current count rejects
# the next honest addition no matter how small, which turns every unrelated
# change into a negotiation with this file and makes switching the gate off the
# path of least resistance. That is not hypothetical: CONTRIBUTING.md was first
# budgeted at its exact size, and the next merge to touch it --- documenting a
# new gate, in good faith --- broke this check.
#
# Each budget is the document's count plus at least 50 words, rounded up to the
# next 50. Headroom is not permission to grow: crossing it should still cost
# something elsewhere in the document.
BUDGETS = {
    "AGENTS.md": 950,
    "CONTRIBUTING.md": 1150,
}
DEFAULT_BUDGET = 1500

# Documents already over budget when this check landed. These may shrink but
# never grow. Delete the entry once the document is under its budget.
#
# Each allowance is the document's word count at the time it was granted,
# rounded up to the next 50 words. The slack is deliberate: an allowance pinned
# to an exact count fails on any edit at all, including one that clarifies
# wording at equal length, and a gate that blocks ordinary editing gets
# switched off. Rounding absorbs that without permitting real growth --- 50
# words on a document this size is under 3%.
ALLOWANCES: dict[str, int] = {
    "docs/runbooks/analyzing-cake-sessions/index.md": 1750,
    ".agents/skills/finding-improvements/SKILL.md": 2750,
}


def find_instruction_files(root: str) -> list[str]:
    """Return sorted repo-relative paths of every instruction document."""
    found: set[str] = set()

    for relpath in INSTRUCTION_FILES:
        if os.path.isfile(os.path.join(root, relpath)):
            found.add(relpath)

    for reldir in INSTRUCTION_DIRS:
        absdir = os.path.join(root, reldir)
        for dirpath, _dirs, files in os.walk(absdir):
            for name in files:
                if name.endswith(".md"):
                    abspath = os.path.join(dirpath, name)
                    found.add(os.path.relpath(abspath, root))

    for reldir in INSTRUCTION_DIRS_SHALLOW:
        absdir = os.path.join(root, reldir)
        if not os.path.isdir(absdir):
            continue
        for name in sorted(os.listdir(absdir)):
            if name.endswith(".md") and os.path.isfile(os.path.join(absdir, name)):
                found.add(os.path.join(reldir, name))

    return sorted(found)


def count_prose_words(filepath: str) -> int:
    """Return the word count of a Markdown file, ignoring fenced code blocks.

    A fence is a line whose first non-space characters are ``` or ~~~. Fences
    toggle in and out of code; the fence lines themselves are not counted.
    """
    words = 0
    fence: str | None = None

    with open(filepath) as f:
        for line in f:
            stripped = line.lstrip()
            if fence is None:
                if stripped.startswith("```") or stripped.startswith("~~~"):
                    fence = stripped[:3]
                    continue
                words += len(line.split())
            elif stripped.startswith(fence):
                fence = None

    return words


def budget_for(relpath: str) -> int:
    """Return the word budget that applies to a document."""
    return BUDGETS.get(relpath, DEFAULT_BUDGET)


def limit_for(relpath: str) -> tuple[int, bool]:
    """Return the enforced limit and whether it comes from an allowance."""
    budget = budget_for(relpath)
    allowance = ALLOWANCES.get(relpath)
    if allowance is not None and allowance > budget:
        return allowance, True
    return budget, False


def main() -> int:
    """Check every instruction document against its budget."""
    script_dir = os.path.dirname(os.path.abspath(__file__))
    root = os.path.dirname(script_dir)

    files = find_instruction_files(root)
    if not files:
        print("ERROR: no instruction documents found", file=sys.stderr)
        return 1

    violations: list[str] = []
    stale: list[str] = []
    total = 0

    for relpath in files:
        count = count_prose_words(os.path.join(root, relpath))
        total += count
        limit, grandfathered = limit_for(relpath)

        if count > limit:
            suffix = " (allowance)" if grandfathered else ""
            violations.append(f"{relpath}: {count} words, limit {limit}{suffix}")
        elif grandfathered and count <= budget_for(relpath):
            stale.append(
                f"{relpath}: {count} words is within its {budget_for(relpath)}-word "
                f"budget; remove its ALLOWANCES entry"
            )

    for line in stale:
        print(f"stale allowance: {line}")

    if violations:
        print("Instruction budget exceeded:", file=sys.stderr)
        for line in violations:
            print(f"  {line}", file=sys.stderr)
        print(
            "\nAdding guidance should displace guidance. Cut something, or raise\n"
            "the budget in scripts/lint-instruction-size.py and say why in the PR.",
            file=sys.stderr,
        )
        return 1

    print(f"Instruction corpus: {total} prose words across {len(files)} documents.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
