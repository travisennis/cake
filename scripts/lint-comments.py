#!/usr/bin/env python3
"""Enforce the ban on ordinary comments in `src/`.

Rationale belongs in `docs/adr/`, not beside the code. Two kinds of comment
stay because tooling consumes them:

- Doc comments (`///`, `//!`), which rustdoc and clap render. The CLI derives
  `--help` text from them, so they are a user-facing compatibility surface.
- `// SAFETY:` blocks, required by `clippy::undocumented_unsafe_blocks`.

Everything else is a violation. The gate is a ratchet: `ci/comment-baseline.json`
records the count still permitted per file. A file may never exceed its entry,
and entries may only fall. A new file starts at zero. `--write-baseline`
records the current counts so a partial cleanup can land without leaving the
gate red.

Usage:
    scripts/lint-comments.py                    # gate against the baseline
    scripts/lint-comments.py --list             # print every current comment
    scripts/lint-comments.py --write-baseline   # rewrite the ratchet baseline

Exit codes: 0 clean, 1 violation or scan error, 2 bad usage.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass

BASELINE = "ci/comment-baseline.json"
SOURCE_DIR = "src"


@dataclass(frozen=True)
class Comment:
    path: str
    line: int
    column: int
    text: str
    is_doc: bool

    @property
    def first_line(self) -> str:
        return self.text.splitlines()[0].strip()


def _skip_quoted(text: str, index: int) -> int:
    """Return the index just past a quoted literal starting at `index`."""
    quote = text[index]
    index += 1
    while index < len(text):
        if text[index] == "\\":
            index += 2
        elif text[index] == quote:
            return index + 1
        else:
            index += 1
    return index


def _raw_string_end(text: str, index: int) -> int | None:
    """Return the index past a raw string at `index`, or None if not one."""
    cursor = index
    if text.startswith(("br", "cr", "r"), cursor):
        cursor += 1 if text[cursor] == "r" else 2
    else:
        return None
    hashes = 0
    while cursor < len(text) and text[cursor] == "#":
        hashes += 1
        cursor += 1
    if cursor >= len(text) or text[cursor] != '"':
        return None
    terminator = '"' + "#" * hashes
    end = text.find(terminator, cursor + 1)
    return len(text) if end == -1 else end + len(terminator)


def _char_end(text: str, index: int) -> int | None:
    """Return the index past a char literal at `index`, or None for a lifetime."""
    cursor = index + 1
    if cursor < len(text) and text[cursor] == "\\":
        cursor += 2
        if cursor < len(text) and text[cursor - 1] == "u" and text[cursor] == "{":
            close = text.find("}", cursor)
            cursor = len(text) if close == -1 else close + 1
    else:
        cursor += 1
    if cursor < len(text) and text[cursor] == "'":
        return cursor + 1
    return None


def _line_starts(text: str) -> list[int]:
    starts = [0]
    for index, char in enumerate(text):
        if char == "\n":
            starts.append(index + 1)
    return starts


def scan(text: str, path: str) -> list[Comment]:
    """Return every doc and ordinary comment in `text`, in source order."""
    comments: list[Comment] = []
    starts = _line_starts(text)
    line_index = 0

    def record(start: int, end: int, is_doc: bool) -> None:
        nonlocal line_index
        while line_index + 1 < len(starts) and starts[line_index + 1] <= start:
            line_index += 1
        comments.append(
            Comment(
                path=path,
                line=line_index + 1,
                column=start - starts[line_index],
                text=text[start:end],
                is_doc=is_doc,
            )
        )

    index = 0
    while index < len(text):
        char = text[index]
        if char == "/" and text.startswith("//", index):
            end = text.find("\n", index)
            end = len(text) if end == -1 else end
            body = text[index + 2 : end]
            is_doc = body.startswith("!") or (
                body.startswith("/") and not body.startswith("//")
            )
            record(index, end, is_doc)
            index = end
        elif char == "/" and text.startswith("/*", index):
            start = index
            depth = 0
            while index < len(text):
                if text.startswith("/*", index):
                    depth += 1
                    index += 2
                elif text.startswith("*/", index):
                    depth -= 1
                    index += 2
                    if depth == 0:
                        break
                else:
                    index += 1
            body = text[start:index]
            is_doc = body.startswith("/**") and not body.startswith("/***")
            is_doc = is_doc or body.startswith("/*!")
            record(start, index, is_doc)
        elif char == '"':
            index = _skip_quoted(text, index)
        elif char == "r" or text.startswith(("br", "cr"), index):
            raw_end = _raw_string_end(text, index)
            index = raw_end if raw_end is not None else index + 1
        elif char == "'":
            char_end = _char_end(text, index)
            index = char_end if char_end is not None else index + 1
        else:
            index += 1
    return comments


def is_safety_block(comments: list[Comment], index: int) -> bool:
    """True when comment `index` belongs to a contiguous `// SAFETY:` block."""
    cursor = index
    while cursor > 0:
        previous = comments[cursor - 1]
        current = comments[cursor]
        if previous.line != current.line - 1 or previous.is_doc:
            break
        cursor -= 1
    return comments[cursor].first_line.startswith("// SAFETY:")


def violations(comments: list[Comment]) -> list[Comment]:
    """Return the comments the ban forbids."""
    result = []
    for index, comment in enumerate(comments):
        if comment.is_doc:
            continue
        if is_safety_block(comments, index):
            continue
        result.append(comment)
    return result


def rust_files(root: str) -> list[str]:
    paths = []
    for dirpath, _dirs, files in os.walk(os.path.join(root, SOURCE_DIR)):
        for name in files:
            if name.endswith(".rs"):
                paths.append(os.path.join(dirpath, name))
    return sorted(paths)


def collect(root: str) -> dict[str, list[Comment]]:
    found: dict[str, list[Comment]] = {}
    for path in rust_files(root):
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        relative = os.path.relpath(path, root)
        remaining = violations(scan(text, relative))
        if remaining:
            found[relative] = remaining
    return found


def load_baseline(path: str) -> dict[str, int]:
    try:
        with open(path, encoding="utf-8") as handle:
            data = json.load(handle)
    except FileNotFoundError:
        print(f"ERROR: comment baseline not found: {path}", file=sys.stderr)
        print("Run scripts/lint-comments.py --write-baseline.", file=sys.stderr)
        raise SystemExit(2)
    entries = data.get("entries", {})
    if not isinstance(entries, dict):
        print(f"ERROR: malformed comment baseline: {path}", file=sys.stderr)
        raise SystemExit(2)
    return {str(key): int(value) for key, value in entries.items()}


def write_baseline(path: str, found: dict[str, list[Comment]]) -> None:
    entries = {file: len(items) for file, items in sorted(found.items())}
    payload = {
        "schema": 1,
        "note": "Ordinary comments still permitted per file. Ratchet down only.",
        "entries": entries,
    }
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2)
        handle.write("\n")
    print(f"Recorded {sum(entries.values())} comments in {len(entries)} files: {path}")


def list_comments(found: dict[str, list[Comment]]) -> None:
    for file, items in found.items():
        for comment in items:
            print(f"{file}:{comment.line}: {comment.first_line}")
    print(f"{sum(len(items) for items in found.values())} ordinary comments remain")


def gate(root: str, baseline_path: str, found: dict[str, list[Comment]]) -> int:
    baseline = load_baseline(baseline_path)
    current = {file: len(items) for file, items in found.items()}
    failed = 0
    stale = 0

    for file in sorted(set(current) | set(baseline)):
        allowed = baseline.get(file, 0)
        actual = current.get(file, 0)
        if actual > allowed:
            failed += actual - allowed
            print(
                f"FAIL: {file}: {actual} ordinary comments, ratchet allows {allowed}",
                file=sys.stderr,
            )
            for comment in found.get(file, [])[allowed:]:
                print(f"  {file}:{comment.line}: {comment.first_line}", file=sys.stderr)
        elif actual < allowed:
            stale += allowed - actual
            print(
                f"Ratchet has {allowed - actual} unused slot(s) in {file}; "
                "run --write-baseline to lock the gain in."
            )

    total = sum(current.values())
    print(
        f"Comment gate: {total} ordinary comments across "
        f"{len(current)} files, {failed} over the ratchet"
    )
    if failed:
        print(
            "Move the rationale into docs/adr/ and delete the comment. "
            "Doc comments and `// SAFETY:` blocks are exempt.",
            file=sys.stderr,
        )
        return 1
    if stale:
        print(f"Ratchet can fall by {stale} line(s).")
    print("PASS: ordinary comments are within the ratchet")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Enforce the comment ban and ratchet on src/.",
    )
    parser.add_argument(
        "--baseline",
        default=BASELINE,
        help=f"ratchet baseline (default: {BASELINE})",
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--list", action="store_true", help="print current comments")
    mode.add_argument(
        "--write-baseline",
        action="store_true",
        help="record the current counts as the ratchet baseline",
    )
    args = parser.parse_args()

    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    found = collect(root)

    if args.list:
        list_comments(found)
        return 0
    if args.write_baseline:
        write_baseline(os.path.join(root, args.baseline), found)
        return 0
    return gate(root, os.path.join(root, args.baseline), found)


if __name__ == "__main__":
    sys.exit(main())
