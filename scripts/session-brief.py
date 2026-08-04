#!/usr/bin/env python3
"""Print the session briefing: open issues by Status, active ExecPlans, recent research.

Replaces the ahm-driven ``ahm prime`` briefing. The issue portion reads the
Cake Backlog project through ``gh``; the ExecPlan and research sections come
from the filesystem and always print, so the script degrades gracefully when
``gh`` is unavailable or unauthenticated.

Exit code:
  0 – always (informational briefing)

Usage:
    python3 scripts/session-brief.py
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
EXEC_PLANS_ACTIVE = PROJECT_ROOT / "docs" / "exec-plans" / "active"
RESEARCH_DIR = PROJECT_ROOT / "docs" / "research"
RESEARCH_CATEGORIES = ("inbox", "investigations", "sources", "topics")
PROJECT_TITLE = "Cake Backlog"
STATUS_ORDER = ("Ready", "In Progress", "Blocked", "Backlog")


def run(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True)


def first_heading(path: Path) -> str:
    """Return the first ``# Heading`` line in *path*, or the filename stem."""
    try:
        for line in path.read_text(encoding="utf-8").splitlines():
            if line.startswith("# "):
                return line[2:].strip()
    except OSError:
        pass
    return path.stem


def issue_briefing() -> str | None:
    """Return the open-issues briefing grouped by Status, or None when gh fails."""
    try:
        owner = json.loads(run(["gh", "repo", "view", "--json", "owner"]).stdout)["owner"]["login"]
        projects = json.loads(
            run(["gh", "project", "list", "--owner", owner, "--format", "json"]).stdout
        )["projects"]
        number = next((p["number"] for p in projects if p["title"] == PROJECT_TITLE), None)
        if number is None:
            return None
        items = json.loads(
            run(
                [
                    "gh", "project", "item-list", str(number), "--owner", owner,
                    "--limit", "200", "--format", "json",
                ]
            ).stdout
        )["items"]
    except (FileNotFoundError, json.JSONDecodeError, KeyError, subprocess.CalledProcessError):
        return None

    by_status: dict[str, list[dict]] = {}
    for item in items:
        by_status.setdefault(item.get("status") or "Backlog", []).append(item)

    lines = [f"## Open Issues ({len(items)})"]
    for status in STATUS_ORDER:
        bucket = by_status.get(status, [])
        if not bucket:
            continue
        if status == "Ready":
            lines.append(f"Ready: {len(bucket)}")
            for item in sorted(bucket, key=lambda i: (i.get("content") or {}).get("number") or 0):
                content = item.get("content") or {}
                num = content.get("number", "?")
                labels = ",".join(item.get("labels", []))
                label_suffix = f" [{labels}]" if labels else ""
                lines.append(f"  {num} {content.get('title', item.get('title', '?'))}{label_suffix}")
        else:
            lines.append(f"{status}: {len(bucket)}")
    return "\n".join(lines)


def exec_plan_briefing() -> str:
    lines = ["## Active ExecPlans"]
    if not EXEC_PLANS_ACTIVE.is_dir():
        lines.append("- (none)")
        return "\n".join(lines)
    plans = sorted(EXEC_PLANS_ACTIVE.glob("*.md"))
    if not plans:
        lines.append("- (none)")
    for path in plans:
        lines.append(f"- {path.name} {first_heading(path)}")
    return "\n".join(lines)


def research_briefing(limit: int = 5) -> str:
    lines = ["## Recent Research"]
    notes: list[tuple[float, Path]] = []
    for category in RESEARCH_CATEGORIES:
        category_dir = RESEARCH_DIR / category
        if category_dir.is_dir():
            notes.extend((p.stat().st_mtime, p) for p in category_dir.glob("*.md"))
    if not notes:
        lines.append("- (none)")
        return "\n".join(lines)
    for _, path in sorted(notes, reverse=True)[:limit]:
        rel = path.relative_to(PROJECT_ROOT)
        lines.append(f"- [{path.parent.name}]({rel}) {first_heading(path)}")
    return "\n".join(lines)


def main() -> int:
    print("root:", PROJECT_ROOT)
    print("workflow: GitHub Issues + docs/workflow (see AGENTS.md routing)")
    print()

    print(issue_briefing() or "## Open Issues\n- (gh unavailable; run `gh issue list --state open`)")
    print()
    print(exec_plan_briefing())
    print()
    print(research_briefing())
    return 0


if __name__ == "__main__":
    sys.exit(main())
