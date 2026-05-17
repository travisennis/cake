#!/usr/bin/env python3
"""Mark a task as Cancelled and move it from active/ to cancelled/.

Usage:
    python3 .agents/scripts/cancel-task.py <task-id>

The script:
  1. Locates `.agents/.tasks/active/<id>.md`.
  2. Sets the front-matter `status:` field to `Cancelled`.
  3. Updates a `**Status:**` line in the body if present.
  4. Moves the file to `.agents/.tasks/cancelled/<id>.md` using `git mv`
     when the file is tracked, otherwise a plain rename.

After running this, run `just task-index` to regenerate the indexes
(the `just task-cancel` recipe does this automatically).
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
TASKS_DIR = ROOT / ".tasks"
ACTIVE_DIR = TASKS_DIR / "active"
CANCELLED_DIR = TASKS_DIR / "cancelled"


def rewrite_status(text: str) -> str:
    """Set front-matter `status:` to `Cancelled` and update `**Status:**` body line."""
    if text.startswith("---\n"):
        end = text.find("\n---\n", 4)
        if end != -1:
            header = text[4:end]
            new_header_lines = []
            saw_status = False
            for line in header.splitlines():
                if re.match(r"\s*status\s*:", line, re.IGNORECASE):
                    new_header_lines.append("status: Cancelled")
                    saw_status = True
                else:
                    new_header_lines.append(line)
            if not saw_status:
                new_header_lines.append("status: Cancelled")
            text = "---\n" + "\n".join(new_header_lines) + "\n---\n" + text[end + 5 :]

    text = re.sub(
        r"^(\*\*Status:\*\*)\s*.+$",
        r"\1 Cancelled",
        text,
        count=1,
        flags=re.MULTILINE,
    )
    return text


def is_git_tracked(path: Path) -> bool:
    result = subprocess.run(
        ["git", "ls-files", "--error-unmatch", str(path)],
        cwd=ROOT.parent,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return result.returncode == 0


def move_file(src: Path, dst: Path) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    if is_git_tracked(src):
        subprocess.run(
            ["git", "mv", str(src), str(dst)],
            cwd=ROOT.parent,
            check=True,
        )
    else:
        shutil.move(str(src), str(dst))


def main() -> int:
    parser = argparse.ArgumentParser(description="Mark a task as Cancelled and move it to cancelled/.")
    parser.add_argument("task_id", help="Task id (filename stem), e.g. 090 or 047a")
    args = parser.parse_args()

    task_id = args.task_id.strip()
    src = ACTIVE_DIR / f"{task_id}.md"
    dst = CANCELLED_DIR / f"{task_id}.md"

    if not src.exists():
        if dst.exists():
            print(f"Task {task_id} is already in cancelled/.", file=sys.stderr)
            return 0
        print(f"error: {src} not found", file=sys.stderr)
        return 1
    if dst.exists():
        print(f"error: {dst} already exists; refusing to overwrite", file=sys.stderr)
        return 1

    text = src.read_text(encoding="utf-8")
    new_text = rewrite_status(text)
    if new_text != text:
        src.write_text(new_text, encoding="utf-8")

    move_file(src, dst)
    print(f"Cancelled task {task_id}: moved to {dst.relative_to(ROOT.parent)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
