#!/usr/bin/env python3
"""Cap the always-loaded instruction document and report the corpus.

The instruction corpus is the prose an agent loads to decide how to work:
AGENTS.md, CONTRIBUTING.md, ARCHITECTURE.md, the topic and workflow documents
under docs/, the runbooks, and the skills. Records are excluded --- ADRs,
ExecPlans, and research notes are history rather than instructions, and
ExecPlans are required to be self-contained, so a length budget would fight
their purpose.

Counting is prose-only: fenced code blocks are stripped before counting, so a
document is not penalised for carrying the commands a reader needs.

The check has one hard rule and two reports:

- AGENTS.md is the only document loaded into every session; the system prompt,
  the skill catalog, and the tool descriptions make up the rest of the loaded
  set. AGENTS.md carries a fixed prose-word cap. Growth there must displace
  growth. On-demand documents (runbooks, skills, topic documents) are
  unconstrained: they cost nothing until a session loads them, and using them
  is justified by the task.
- Everything else is a report: corpus total, per-document counts, and the
  largest documents, so review can see where the weight accumulates. No
  per-document budgets: an arbitrary number per document just gets renegotiated
  on contact.
- Model-visible prompt assets are reported separately from the instruction
  corpus. Tool descriptions and the system prompt are not instructions and the
  cap does not apply to them, but they reach the model on every request, so
  their cost belongs in the same report instead of growing uncounted. They stay
  out of the corpus total so the two costs remain distinguishable.

Exit code is 1 when AGENTS.md exceeds its cap.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

# Policy cap on AGENTS.md in prose words. AGENTS.md is the one document loaded
# into every session, so its size is a per-session cost: 1200 words is roughly
# 1.6k tokens, about 1.2% of a 128k-token context. The number is policy, not
# derived from the document's current size; there is no raise-by-comment path.
# Contract documentation belongs in the on-demand documents, which are not
# capped.
AGENTS_CAP = 1200

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
# report the day it lands, without pulling in the record subdirectories.
INSTRUCTION_DIRS_SHALLOW = [
    "docs",
]

# Model-visible prompt assets, relative to the repo root. These are not
# instructions and carry no budget: they are reported because they are injected
# into every model request, so their growth is a per-request cost. Snapshots are
# excluded --- they are generated test output, not prompt text.
PROMPT_ASSET_GLOBS = [
    "src/clients/tools/*-description.txt",
    "src/prompts/*.md",
]

# Individual instruction files relative to the repo root.
INSTRUCTION_FILES = [
    "AGENTS.md",
    "CONTRIBUTING.md",
    "ARCHITECTURE.md",
]


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


def find_prompt_assets(root: str) -> list[str]:
    """Return sorted repo-relative paths of the model-visible prompt assets."""
    found: set[str] = set()

    for pattern in PROMPT_ASSET_GLOBS:
        for path in Path(root).glob(pattern):
            if path.is_file():
                found.add(path.relative_to(root).as_posix())

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


def main() -> int:
    """Check AGENTS.md against its cap and report the corpus."""
    script_dir = os.path.dirname(os.path.abspath(__file__))
    root = os.path.dirname(script_dir)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--skill-catalog", metavar="CAKE_BINARY",
        help="also run the native read-only skill catalog report with this Cake binary",
    )
    args = parser.parse_args()

    files = find_instruction_files(root)
    if not files:
        print("ERROR: no instruction documents found", file=sys.stderr)
        return 1

    counts = {
        relpath: count_prose_words(os.path.join(root, relpath)) for relpath in files
    }
    total = sum(counts.values())

    prompt_assets = find_prompt_assets(root)
    prompt_counts = {
        relpath: count_prose_words(os.path.join(root, relpath))
        for relpath in prompt_assets
    }
    prompt_total = sum(prompt_counts.values())

    agents_count = counts.get("AGENTS.md")
    violations: list[str] = []
    if agents_count is not None and agents_count > AGENTS_CAP:
        violations.append(f"AGENTS.md: {agents_count} prose words, cap {AGENTS_CAP}")

    print(f"Instruction corpus: {total} prose words across {len(files)} documents.")
    for relpath, count in sorted(counts.items(), key=lambda item: (-item[1], item[0])):
        suffix = f" (cap {AGENTS_CAP})" if relpath == "AGENTS.md" else ""
        print(f"  {count:6d}  {relpath}{suffix}")

    if prompt_counts:
        print(
            f"\nModel-visible prompt assets: {prompt_total} prose words across "
            f"{len(prompt_counts)} files. Not instructions and not capped; "
            f"reported because they reach the model on every request."
        )
        for relpath, count in sorted(
            prompt_counts.items(), key=lambda item: (-item[1], item[0])
        ):
            print(f"  {count:6d}  {relpath}")

    if args.skill_catalog:
        binary = os.path.abspath(args.skill_catalog)
        try:
            result = subprocess.run([binary, "debug", "skills"], cwd=root, check=False)
        except OSError as error:
            print(
                f"ERROR: could not run skill catalog binary '{binary}': {error.strerror}",
                file=sys.stderr,
            )
            return 1
        if result.returncode:
            return result.returncode

    if violations:
        print("Instruction cap exceeded:", file=sys.stderr)
        for line in violations:
            print(f"  {line}", file=sys.stderr)
        print(
            "\nAGENTS.md loads into every session; growth there must displace "
            "growth. Cut something, or move the guidance to an on-demand "
            "document, which is not capped.",
            file=sys.stderr,
        )
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
