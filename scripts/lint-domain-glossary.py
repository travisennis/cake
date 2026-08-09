#!/usr/bin/env python3
"""Verify that docs/domain-glossary.md stays anchored to the code it explains.

The glossary exists to stop a survey reporting two code paths as duplicates
when they answer different questions. That only works while its entries
describe symbols that still exist, so each entry declares the symbols it
covers on an **Anchors:** line and this check resolves them against `src/`.

An entry whose anchors have all disappeared is describing code that is gone.
That is the staleness the check catches: renames and deletions surface as a
failing gate rather than as prose nobody re-reads.

Each entry must also cite the issue or pull request that forced it. An entry
with no provenance was written speculatively, which the glossary's own
"when to add an entry" rule forbids.

Exit code is 0 when clean and 1 when any entry fails.
"""

from __future__ import annotations

import os
import re
import sys

GLOSSARY = "docs/domain-glossary.md"
SOURCE_DIR = "src"

# Headings before this one are front matter for the glossary itself, not
# entries. Everything after it, at depth 3, is an entry.
ENTRIES_HEADING = "## Entries"

ANCHOR_LINE = re.compile(r"^\*\*Anchors:\*\*\s*(.+)$")
PROVENANCE_LINE = re.compile(r"^\*.*#(\d+).*\*$")
CODE_SPAN = re.compile(r"`([A-Za-z_][A-Za-z0-9_]*)`")


def read_source_identifiers(root: str) -> set[str]:
    """Return every identifier-like token appearing in the Rust sources."""
    tokens: set[str] = set()
    src = os.path.join(root, SOURCE_DIR)
    for dirpath, _dirs, files in os.walk(src):
        for name in files:
            if not name.endswith(".rs"):
                continue
            with open(os.path.join(dirpath, name), encoding="utf-8") as handle:
                for line in handle:
                    tokens.update(re.findall(r"[A-Za-z_][A-Za-z0-9_]*", line))
    return tokens


def parse_entries(text: str) -> list[tuple[str, list[str], str | None]]:
    """Return (title, anchors, provenance) for each `###` entry."""
    body = text.split(ENTRIES_HEADING, 1)
    if len(body) != 2:
        return []

    entries: list[tuple[str, list[str], str | None]] = []
    title: str | None = None
    anchors: list[str] = []
    provenance: str | None = None

    def flush() -> None:
        if title is not None:
            entries.append((title, anchors, provenance))

    for line in body[1].splitlines():
        stripped = line.strip()
        if stripped.startswith("## "):
            break  # a new top-level section ends the entry list
        if stripped.startswith("### "):
            flush()
            title, anchors, provenance = stripped[4:].strip(), [], None
            continue
        if title is None:
            continue
        match = ANCHOR_LINE.match(stripped)
        if match:
            anchors = CODE_SPAN.findall(match.group(1))
        elif PROVENANCE_LINE.match(stripped):
            provenance = stripped

    flush()
    return entries


def main() -> int:
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    path = os.path.join(root, GLOSSARY)
    if not os.path.isfile(path):
        print(f"{GLOSSARY}: not found", file=sys.stderr)
        return 1

    with open(path, encoding="utf-8") as handle:
        entries = parse_entries(handle.read())

    if not entries:
        print(f"{GLOSSARY}: no entries found under '{ENTRIES_HEADING}'", file=sys.stderr)
        return 1

    identifiers = read_source_identifiers(root)
    failures: list[str] = []

    for title, anchors, provenance in entries:
        if not anchors:
            failures.append(f"{title!r}: no **Anchors:** line")
            continue
        missing = [a for a in anchors if a not in identifiers]
        if len(missing) == len(anchors):
            failures.append(
                f"{title!r}: every anchor is gone from {SOURCE_DIR}/ "
                f"({', '.join(missing)}) --- the entry describes code that no "
                f"longer exists"
            )
        elif missing:
            failures.append(
                f"{title!r}: anchors not found in {SOURCE_DIR}/: {', '.join(missing)}"
            )
        if provenance is None:
            failures.append(f"{title!r}: no issue or pull request cited")

    if failures:
        print("Domain glossary is out of date:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        print(
            "\nAn entry must describe code that exists and cite the decision "
            "that forced it.\nUpdate the entry, or remove it if the "
            "distinction it drew no longer applies.",
            file=sys.stderr,
        )
        return 1

    anchor_count = sum(len(a) for _, a, _ in entries)
    print(
        f"Domain glossary: {len(entries)} entries, {anchor_count} anchors "
        f"resolved against {SOURCE_DIR}/."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
